//! `node.camera_lens` exposure proof (CAMERA_AND_LENS_DESIGN.md §2 D5, §4
//! P2 gate).
//!
//! A single unlit white quad, lit only by its own emission-free albedo (no
//! lights needed — `fs_unlit` has no light loop), viewed through
//! `node.orbit_camera` with a `node.camera_lens` spliced in front of
//! `node.render_scene`. Proves the one thing the unit tests can't reach
//! through the real render path: `render_scene`'s fragment shaders actually
//! multiply their output by `exp2(exposure_ev)`, reading the value from
//! `scene_params.z` end-to-end (CPU camera_lens param → Camera wire →
//! render_scene uniform → WGSL).
//!
//! `ev_one_doubles_ev_zero`: `exposure_ev = 1.0` must read back ~2.0× the
//! `exposure_ev = 0.0` render (within f16 tolerance).

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// A single unlit white quad (grid_mesh's XZ-plane grid) viewed from a
/// tilted-down orbit camera so the plane fills the frame (not edge-on).
/// `lens_ev` is `Some(ev)` to
/// splice a `node.camera_lens` between the camera and `render_scene`, wired
/// at that `exposure_ev` (every other lens param left at its neutral
/// default); `None` wires the camera directly into `render_scene` — no
/// `camera_lens` node in the graph at all.
fn quad_scene_json(lens_ev: Option<f32>) -> String {
    let mut nodes = String::from(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"quad_grid","params":{
            "max_capacity":{"type":"Int","value":16},
            "resolution_x":{"type":"Int","value":2},
            "resolution_y":{"type":"Int","value":2},
            "size_x":{"type":"Float","value":4.0},
            "size_y":{"type":"Float","value":4.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"quad_tris","params":{
            "src_cols":{"type":"Int","value":2},
            "src_rows":{"type":"Int","value":2}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.0},
            "tilt":{"type":"Float","value":0.6},
            "distance":{"type":"Float","value":8.0},
            "fov_y":{"type":"Float","value":0.9}}},
        {"id":4,"typeId":"node.unlit_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "color_a":{"type":"Float","value":1.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":0}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}"#,
    );

    let mut wires = String::from(
        r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#,
    );

    match lens_ev {
        Some(ev) => {
            nodes.push_str(&format!(
                r#",{{"id":10,"typeId":"node.camera_lens","nodeId":"lens","params":{{
                    "exposure_ev":{{"type":"Float","value":{ev}}}}}}}"#,
            ));
            wires.push_str(
                r#",{"fromNode":3,"fromPort":"out","toNode":10,"toPort":"camera"},
                {"fromNode":10,"fromPort":"out","toNode":20,"toPort":"camera"}"#,
            );
        }
        None => {
            wires.push_str(r#",{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}"#);
        }
    }

    format!(r#"{{"version":2,"name":"RenderSceneExposureProof","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

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
    .expect("exposure quad scene graph must build");

    let target = h.make_target("render-scene-exposure");
    for frame in 0..2 {
        let ctx = PresetContext {
            time: 0.1,
            beat: 0.2,
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
        let mut enc = h.device.create_encoder("render-scene-exposure-enc");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
            runtime.render(
                &mut gpu,
                &target.texture,
                &ctx,
                &manifold_core::params::ParamManifest::default(),
            );
        }
        enc.commit_and_wait_completed();
    }
    h.readback(&target.texture)
}

/// Mean (r, g, b) over lit (non-black) pixels of an `Rgba16Float` readback.
fn mean_lit_rgb(bytes: &[u8]) -> (f64, f64, f64) {
    let (mut sr, mut sg, mut sb, mut n) = (0.0f64, 0.0f64, 0.0f64, 0u64);
    for px in bytes.chunks_exact(8) {
        let r = f16::from_le_bytes([px[0], px[1]]).to_f32();
        let g = f16::from_le_bytes([px[2], px[3]]).to_f32();
        let b = f16::from_le_bytes([px[4], px[5]]).to_f32();
        assert!(r.is_finite() && g.is_finite() && b.is_finite(), "non-finite pixel");
        if r + g + b > 0.02 {
            sr += r as f64;
            sg += g as f64;
            sb += b as f64;
            n += 1;
        }
    }
    let n = n.max(1) as f64;
    (sr / n, sg / n, sb / n)
}

#[test]
fn ev_one_doubles_ev_zero_within_f16_tolerance() {
    let ev0 = render_readback(&quad_scene_json(Some(0.0)));
    let ev1 = render_readback(&quad_scene_json(Some(1.0)));

    let (r0, g0, b0) = mean_lit_rgb(&ev0);
    let (r1, g1, b1) = mean_lit_rgb(&ev1);
    eprintln!("ev=0 mean rgb = ({r0:.4},{g0:.4},{b0:.4})");
    eprintln!("ev=1 mean rgb = ({r1:.4},{g1:.4},{b1:.4})");

    assert!(r0 > 0.05, "ev=0 quad should read back lit, got r={r0:.4}");
    // exp2(1.0) == 2.0 exactly; f16 has ~3 decimal digits of precision at
    // this magnitude, so 2% relative tolerance is generous, not loose.
    for (c0, c1, name) in [(r0, r1, "r"), (g0, g1, "g"), (b0, b1, "b")] {
        let ratio = c1 / c0.max(1e-6);
        assert!(
            (ratio - 2.0).abs() < 0.04,
            "channel {name}: ev=1/ev=0 ratio should be ~2.0, got {ratio:.4} (ev0={c0:.4}, ev1={c1:.4})"
        );
    }
}

#[test]
fn ev_zero_camera_lens_is_byte_identical_to_no_camera_lens() {
    // I5 (docs/CAMERA_AND_LENS_DESIGN.md §3): a camera_lens at ev=0 must be
    // a pure no-op, indistinguishable from not wiring camera_lens at all —
    // proven at the pixel level on the exposure-carrying scene itself, not
    // just asserted from the uniform math.
    let with_lens = render_readback(&quad_scene_json(Some(0.0)));
    let without_lens = render_readback(&quad_scene_json(None));
    assert_eq!(
        with_lens, without_lens,
        "camera_lens at exposure_ev=0 must be byte-identical to no camera_lens node"
    );
}
