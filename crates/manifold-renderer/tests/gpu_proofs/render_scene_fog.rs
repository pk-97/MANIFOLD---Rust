//! `node.render_scene` atmosphere / depth-fog proof (REALTIME_3D_DESIGN §5
//! P3 gate).
//!
//! Two things the unit tests can't reach: (1) that a wired `node.atmosphere`
//! actually tints distant geometry toward the fog colour through the real
//! render path, and (2) that `fog_density == 0` is byte-identical to having
//! no atmosphere at all — the "unwired = zero cost" contract, proven at the
//! pixel level, not asserted.
//!
//! Scene: a large ground plane viewed at a grazing angle so its far edge is
//! many units from the camera. A distinctly-BLUE fog is wired at moderate
//! density; distant pixels must gain blue relative to the same scene with no
//! fog. The second test renders the scene with an atmosphere wired at density
//! 0 and again with no atmosphere node, and asserts the readbacks are
//! byte-for-byte equal.

use half::f16;
use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// A large ground plane lit by one overhead sun, viewed at a grazing angle.
/// `fog` is `Some((density, r, g, b))` to wire a `node.atmosphere`, or `None`
/// for no atmosphere node at all.
fn fog_scene_json(fog: Option<(f32, f32, f32, f32)>) -> String {
    let mut nodes = String::from(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":16384},
            "resolution_x":{"type":"Int","value":32},
            "resolution_y":{"type":"Int","value":32},
            "size_x":{"type":"Float","value":40.0},
            "size_y":{"type":"Float","value":40.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":32},
            "src_rows":{"type":"Int","value":32}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.0},
            "tilt":{"type":"Float","value":0.12},
            "distance":{"type":"Float","value":15.0},
            "fov_y":{"type":"Float","value":1.0}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.1}}},
        {"id":30,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":0.0},
            "pos_y":{"type":"Float","value":30.0},
            "pos_z":{"type":"Float","value":0.0},
            "aim_x":{"type":"Float","value":0.0},
            "aim_y":{"type":"Float","value":0.0},
            "aim_z":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":0.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}"#,
    );

    let mut wires = String::from(
        r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#,
    );

    if let Some((density, r, g, b)) = fog {
        nodes.push_str(&format!(
            r#",{{"id":40,"typeId":"node.atmosphere","nodeId":"atmo","params":{{
                "fog_color_r":{{"type":"Float","value":{r}}},
                "fog_color_g":{{"type":"Float","value":{g}}},
                "fog_color_b":{{"type":"Float","value":{b}}},
                "fog_density":{{"type":"Float","value":{density}}},
                "height_falloff":{{"type":"Float","value":0.0}}}}}}"#,
        ));
        wires.push_str(r#",{"fromNode":40,"fromPort":"atmosphere","toNode":20,"toPort":"atmosphere"}"#);
    }

    format!(r#"{{"version":2,"name":"RenderSceneFogProof","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

/// Same ground-plane scene as [`fog_scene_json`], no fog, with a
/// `node.atmosphere` wired whose ONLY non-default field is `shaft_intensity`
/// (VOLUMETRIC_LIGHT_DESIGN.md D1/D2). At `shaft_intensity == 0` (the
/// default/unwired value) this must still be byte-identical to no atmosphere
/// at all (V1) — the march never runs. Non-zero values now (P2) drive the
/// real march kernel.
fn shaft_scene_json(shaft_intensity: f32) -> String {
    let mut nodes = String::from(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":16384},
            "resolution_x":{"type":"Int","value":32},
            "resolution_y":{"type":"Int","value":32},
            "size_x":{"type":"Float","value":40.0},
            "size_y":{"type":"Float","value":40.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":32},
            "src_rows":{"type":"Int","value":32}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.0},
            "tilt":{"type":"Float","value":0.12},
            "distance":{"type":"Float","value":15.0},
            "fov_y":{"type":"Float","value":1.0}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.1}}},
        {"id":30,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":0.0},
            "pos_y":{"type":"Float","value":30.0},
            "pos_z":{"type":"Float","value":0.0},
            "aim_x":{"type":"Float","value":0.0},
            "aim_y":{"type":"Float","value":0.0},
            "aim_z":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":0.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1}}}"#,
    );
    nodes.push_str(&format!(
        r#",{{"id":40,"typeId":"node.atmosphere","nodeId":"atmo","params":{{
            "shaft_intensity":{{"type":"Float","value":{shaft_intensity}}}}}}}"#,
    ));
    nodes.push_str(r#",{"id":99,"typeId":"system.final_output","nodeId":"out"}"#);

    let wires = r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":40,"fromPort":"atmosphere","toNode":20,"toPort":"atmosphere"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#;

    format!(r#"{{"version":2,"name":"RenderSceneShaftProof","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

#[test]
fn shafts_off_byte_identical() {
    // VOLUMETRIC_LIGHT_DESIGN.md V1 (re-run at P2): shaft_intensity == 0
    // (default, unwired) must render byte-identical to today, even now that
    // the march kernel exists — `wants_shafts` gates it off entirely, no
    // texture/pipeline is ever touched.
    let (off, _, _) = render_readback(&shaft_scene_json(0.0));
    let (golden, _, _) = render_readback(&fog_scene_json(None));
    assert_eq!(
        off, golden,
        "shaft_intensity 0 must be byte-identical to no atmosphere at all (the pre-change golden buffer)"
    );

    // The inverse of the old P1-only assertion: now that the march kernel
    // exists, a non-zero shaft_intensity MUST change pixels (there's a lit
    // Sun with cast_shadows off — unshadowed glow — so a nonzero fog_density
    // is needed for the march to have anything to scatter through; the
    // scene here has fog_density 0 via `node.atmosphere`'s default, so the
    // march's sigma is 0 everywhere and it's STILL a no-op — that's D1's own
    // "one haze, two renderings" contract, not a P2 regression). Assert the
    // real positive case instead: wiring both fog_density and shaft_intensity
    // must move pixels away from the golden buffer.
    let hazy = shaft_and_fog_scene_json(0.15, 5.0);
    let (hazy_bytes, _, _) = render_readback(&hazy);
    assert_ne!(
        hazy_bytes, golden,
        "fog_density>0 AND shaft_intensity>0 must move pixels — the march kernel must actually run (P2)"
    );
}

/// Same ground-plane scene as [`shaft_scene_json`], but the `node.atmosphere`
/// also carries a nonzero `fog_density` — the march's `σ(x)` needs density to
/// have anything to scatter (D2: `σ = fog_density · exp(-height_falloff ·
/// max(x.y,0))`), so this is the minimal scene that actually exercises the
/// march kernel end-to-end.
fn shaft_and_fog_scene_json(fog_density: f32, shaft_intensity: f32) -> String {
    let mut nodes = String::from(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":16384},
            "resolution_x":{"type":"Int","value":32},
            "resolution_y":{"type":"Int","value":32},
            "size_x":{"type":"Float","value":40.0},
            "size_y":{"type":"Float","value":40.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":32},
            "src_rows":{"type":"Int","value":32}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.0},
            "tilt":{"type":"Float","value":0.12},
            "distance":{"type":"Float","value":15.0},
            "fov_y":{"type":"Float","value":1.0}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.1}}},
        {"id":30,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":0.0},
            "pos_y":{"type":"Float","value":30.0},
            "pos_z":{"type":"Float","value":0.0},
            "aim_x":{"type":"Float","value":0.0},
            "aim_y":{"type":"Float","value":0.0},
            "aim_z":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":1.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1}}}"#,
    );
    nodes.push_str(&format!(
        r#",{{"id":40,"typeId":"node.atmosphere","nodeId":"atmo","params":{{
            "fog_density":{{"type":"Float","value":{fog_density}}},
            "shaft_intensity":{{"type":"Float","value":{shaft_intensity}}}}}}}"#,
    ));
    nodes.push_str(r#",{"id":99,"typeId":"system.final_output","nodeId":"out"}"#);

    let wires = r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":40,"fromPort":"atmosphere","toNode":20,"toPort":"atmosphere"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#;

    format!(r#"{{"version":2,"name":"RenderSceneShaftFogProof","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

#[test]
fn shafts_two_runs_bit_identical() {
    // VOLUMETRIC_LIGHT_DESIGN.md V2: the journey-proofs determinism
    // property, scene-local. Committed hash jitter + fixed step counts +
    // no temporal accumulation (D5) means two independent renders of the
    // SAME graph must be bit-for-bit identical — not merely close.
    let json = shaft_and_fog_scene_json(0.15, 1.2);
    let (first, _, _) = render_readback(&json);
    let (second, _, _) = render_readback(&json);
    assert_eq!(first, second, "two runs of the same shaft graph must be bit-identical (D5, no temporal accumulation)");
}

#[test]
fn shafts_leave_alpha_untouched() {
    // VOLUMETRIC_LIGHT_DESIGN.md V4: the additive composite's alpha channel
    // must be untouched by the shaft blend (D3) — the ROP's src_alpha=Zero/
    // dst_alpha=One enforces this, this test proves it at the pixel.
    let on = shaft_and_fog_scene_json(0.15, 1.2);
    let off = shaft_and_fog_scene_json(0.15, 0.0);
    let (on_bytes, _, _) = render_readback(&on);
    let (off_bytes, _, _) = render_readback(&off);
    assert_eq!(on_bytes.len(), off_bytes.len());
    for (i, chunk) in on_bytes.chunks_exact(8).enumerate() {
        let off_chunk = &off_bytes[i * 8..i * 8 + 8];
        let on_a = f16::from_le_bytes([chunk[6], chunk[7]]).to_f32();
        let off_a = f16::from_le_bytes([off_chunk[6], off_chunk[7]]).to_f32();
        assert_eq!(
            on_a, off_a,
            "pixel {i}: alpha must be untouched by the shaft composite (shafts-on {on_a} vs shafts-off {off_a})"
        );
    }
}

/// Sum of every channel over EVERY pixel (no "lit pixel" filtering). The
/// composite is a pure additive blend (`color.rgb += result`, never
/// subtracts, D3) and the march's own output scales linearly with
/// `shaft_intensity` (D2's `out = L * shaft_intensity * exp2(exposure_ev)`,
/// every term in `L` non-negative) — so this total is the metric that MUST
/// be monotonic. A "mean over lit (>0.02) pixels" metric is the wrong
/// oracle here: turning shafts on pulls previously-excluded near-black
/// background pixels into the filtered set, which can lower a filtered mean
/// even though every individual pixel's value only went up.
fn total_rgb_sum(bytes: &[u8]) -> f64 {
    let mut sum = 0.0f64;
    for px in bytes.chunks_exact(8) {
        for c in 0..3 {
            let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
            assert!(v.is_finite(), "non-finite pixel channel");
            sum += v as f64;
        }
    }
    sum
}

#[test]
fn shaft_intensity_is_a_monotonic_performer_fader() {
    // Performer-gesture line (P2): `shaft_intensity` is a card-driven fader —
    // driving it across a range must monotonically increase the frame's
    // total additive-light response (see `total_rgb_sum` for why this is
    // the correct metric, not a filtered mean).
    let mut prev_sum = -1.0f64;
    for intensity in [0.0f32, 0.3, 0.8, 1.5, 3.0] {
        let json = shaft_and_fog_scene_json(0.15, intensity);
        let (bytes, _, _) = render_readback(&json);
        let sum = total_rgb_sum(&bytes);
        assert!(
            sum >= prev_sum - 1e-3,
            "shaft_intensity {intensity}: total rgb sum {sum:.4} must be >= previous {prev_sum:.4} (monotonic fader)"
        );
        prev_sum = sum;
    }
    assert!(prev_sum > 0.0, "sanity: the final (hottest) frame must have nonzero total light");
}

fn render_readback(json: &str) -> (Vec<u8>, u32, u32) {
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
    .expect("fog scene graph must build");

    let target = h.make_target("render-scene-fog");
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
        let mut enc = h.device.create_encoder("render-scene-fog-enc");
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
    (h.readback(&target.texture), h.width, h.height)
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

fn write_png(bytes: &[u8], w: u32, h: u32, path: &str) {
    let mut out = Vec::with_capacity((w * h * 4) as usize);
    for px in bytes.chunks_exact(8) {
        for c in 0..4 {
            let v = f16::from_le_bytes([px[c * 2], px[c * 2 + 1]]).to_f32();
            let mapped = (v / (1.0 + v)).clamp(0.0, 1.0);
            out.push((mapped.powf(1.0 / 2.2) * 255.0).round() as u8);
        }
    }
    image::save_buffer(path, &out, w, h, image::ExtendedColorType::Rgba8)
        .unwrap_or_else(|e| panic!("write {path}: {e}"));
}

#[test]
fn blue_fog_tints_the_scene_toward_the_fog_color() {
    // Distinct blue fog vs no fog. Fog can only ADD blue and REMOVE
    // white-lit red/green as distance grows, so mean blue must rise and the
    // scene must read bluer (b/r ratio up).
    let (fog_bytes, w, h) = render_readback(&fog_scene_json(Some((0.06, 0.1, 0.3, 0.9))));
    let (clear_bytes, _, _) = render_readback(&fog_scene_json(None));

    write_png(&fog_bytes, w, h, "/tmp/render_scene_fog_on.png");
    write_png(&clear_bytes, w, h, "/tmp/render_scene_fog_off.png");

    let (fr, fg, fb) = mean_lit_rgb(&fog_bytes);
    let (cr, cg, cb) = mean_lit_rgb(&clear_bytes);
    eprintln!("fog  mean rgb = ({fr:.3},{fg:.3},{fb:.3})");
    eprintln!("clear mean rgb = ({cr:.3},{cg:.3},{cb:.3})");

    // The clear scene is white-lit (r≈g≈b). Blue fog blends distant geometry
    // toward (0.1,0.3,0.9): a sub-white colour, so it lowers ALL channels,
    // but far more in red/green than blue — leaving BLUE the dominant channel
    // where it was tied before. That flip is the decisive readout of fog.
    assert!(cr > 0.2 && (cr - cb).abs() < 0.05, "clear scene should be ~neutral white");
    assert!(fb > fr + 0.05 && fb > fg + 0.02, "blue fog must make blue the dominant channel: fog rgb=({fr:.3},{fg:.3},{fb:.3})");
    // And the blue/red balance shifts markedly bluer than the clear scene.
    assert!(
        fb / fr.max(1e-4) > cb / cr.max(1e-4) + 0.1,
        "fog must shift the blue/red balance toward blue: \
         fog b/r={:.3} clear b/r={:.3}",
        fb / fr.max(1e-4),
        cb / cr.max(1e-4)
    );
}

#[test]
fn density_zero_atmosphere_is_byte_identical_to_no_atmosphere() {
    // Atmosphere wired at density 0 (all-default node.atmosphere) must be a
    // pure no-op — byte-for-byte identical to a graph with no atmosphere node
    // at all. This is the "unwired / off = zero cost" contract, at the pixel.
    let (with_zero, _, _) = render_readback(&fog_scene_json(Some((0.0, 0.5, 0.55, 0.65))));
    let (without, _, _) = render_readback(&fog_scene_json(None));
    assert_eq!(
        with_zero, without,
        "density-0 atmosphere must be byte-identical to no atmosphere"
    );
}

/// Same ground-plane scene as [`fog_scene_json`] (no atmosphere), but with a
/// `node.camera_lens` spliced between `cam` and `scene` when `lens_ev` is
/// `Some` — wired at that `exposure_ev`, every other lens param left at its
/// neutral default. `None` wires the camera directly into `render_scene`,
/// matching `fog_scene_json(None)`'s shape exactly (no `camera_lens` node in
/// the graph at all).
fn fog_scene_json_with_lens(lens_ev: Option<f32>) -> String {
    let mut nodes = String::from(
        r#"{"id":0,"typeId":"system.generator_input","nodeId":"input"},
        {"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{
            "max_capacity":{"type":"Int","value":16384},
            "resolution_x":{"type":"Int","value":32},
            "resolution_y":{"type":"Int","value":32},
            "size_x":{"type":"Float","value":40.0},
            "size_y":{"type":"Float","value":40.0}}},
        {"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{
            "src_cols":{"type":"Int","value":32},
            "src_rows":{"type":"Int","value":32}}},
        {"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{
            "orbit":{"type":"Float","value":0.0},
            "tilt":{"type":"Float","value":0.12},
            "distance":{"type":"Float","value":15.0},
            "fov_y":{"type":"Float","value":1.0}}},
        {"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "ambient":{"type":"Float","value":0.1}}},
        {"id":30,"typeId":"node.light","nodeId":"sun","params":{
            "mode":{"type":"Enum","value":0},
            "pos_x":{"type":"Float","value":0.0},
            "pos_y":{"type":"Float","value":30.0},
            "pos_z":{"type":"Float","value":0.0},
            "aim_x":{"type":"Float","value":0.0},
            "aim_y":{"type":"Float","value":0.0},
            "aim_z":{"type":"Float","value":0.0},
            "color_r":{"type":"Float","value":1.0},
            "color_g":{"type":"Float","value":1.0},
            "color_b":{"type":"Float","value":1.0},
            "intensity":{"type":"Float","value":1.0},
            "cast_shadows":{"type":"Float","value":0.0}}},
        {"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{
            "objects":{"type":"Int","value":1},
            "lights":{"type":"Int","value":1}}},
        {"id":99,"typeId":"system.final_output","nodeId":"out"}"#,
    );

    let mut wires = String::from(
        r#"{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"},
        {"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"},
        {"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"},
        {"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"},
        {"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}"#,
    );

    match lens_ev {
        Some(ev) => {
            nodes.push_str(&format!(
                r#",{{"id":40,"typeId":"node.camera_lens","nodeId":"lens","params":{{
                    "exposure_ev":{{"type":"Float","value":{ev}}}}}}}"#,
            ));
            wires.push_str(
                r#",{"fromNode":3,"fromPort":"out","toNode":40,"toPort":"camera"},
                {"fromNode":40,"fromPort":"out","toNode":20,"toPort":"camera"}"#,
            );
        }
        None => {
            wires.push_str(r#",{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}"#);
        }
    }

    format!(r#"{{"version":2,"name":"RenderSceneLensProof","nodes":[{nodes}],"wires":[{wires}]}}"#)
}

#[test]
fn ev_zero_camera_lens_is_byte_identical_to_no_camera_lens() {
    // I5 (docs/CAMERA_AND_LENS_DESIGN.md §3): extends this file's density-0
    // byte-identity contract to camera_lens's exposure_ev — a camera_lens
    // wired at ev=0 must render byte-for-byte identical to not wiring
    // camera_lens at all, same "unwired/neutral = zero cost" shape as
    // `density_zero_atmosphere_is_byte_identical_to_no_atmosphere` above.
    let with_zero_ev = render_readback(&fog_scene_json_with_lens(Some(0.0)));
    let without_lens = render_readback(&fog_scene_json_with_lens(None));
    assert_eq!(
        with_zero_ev, without_lens,
        "camera_lens at exposure_ev=0 must be byte-identical to no camera_lens node"
    );
}
