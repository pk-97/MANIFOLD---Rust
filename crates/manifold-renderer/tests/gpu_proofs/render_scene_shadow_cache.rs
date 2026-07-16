//! `node.render_scene` shadow-map CACHING proof
//! (RENDER_SCENE_PERF_OPTIMIZATION_DESIGN.md P2 — I1/I4 gate).
//!
//! P2 adds a per-caster dirty-check key (D6) that skips the depth-only
//! shadow batch when nothing the shadow pass reads has changed since the
//! last real render. What these tests prove, on the real GPU path (the
//! isolated Rust hash-key logic can't reach the actual persisted texture
//! content):
//!
//! - **I4** (`static_scene_frame_30_matches_fresh_executor_frame_1`): a
//!   scene with no time-varying input renders IDENTICAL output whether the
//!   frame comes from a long-running executor that has been serving its
//!   shadow map from cache for 29 frames, or from a brand-new executor's
//!   very first frame (which never hits the cache — `shadow_cache_keys`
//!   starts at `None`). Bit-for-bit equality proves the cache is invisible.
//! - **I1** (the mutation trio): a light-param change, an object-transform
//!   change, and a mesh-content change (a source param, not a topology
//!   change — same vertex COUNT, different vertex DATA) each force a real
//!   re-render on the very next frame, whose output is bit-identical to a
//!   FRESH executor rendering that same mutated state cold. Any key
//!   component that failed to invalidate would show up as a diff here.
//!
//! Reuses `render_scene_shadows.rs`'s ground+occluder+one-Sun fixture shape
//! (that file already proves the shadow pass itself darkens the ground;
//! this file proves CACHING it doesn't change what gets rendered). The
//! `node.beat_ramp -> node.transform_3d`/`node.light`/`node.grid_mesh`
//! port-shadow wiring pattern mirrors `gbuffer_velocity.rs`'s beat-driven
//! mutation, keeping ONE `PresetRuntime` (and so one `RenderScene` node
//! instance and one `Executor`) alive across every render call — the
//! continuity the cache itself depends on.

use manifold_gpu::GpuTextureFormat;
use manifold_renderer::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use manifold_renderer::node_graph::PrimitiveRegistry;
use manifold_renderer::preset_context::PresetContext;
use manifold_renderer::preset_runtime::PresetRuntime;

use crate::harness;

/// Which param the `beat_ramp` output port-shadows, one wire added to the
/// otherwise-identical base scene. `None` renders the fully static scene
/// (I4's fixture) — no `beat_ramp` node is even wired in that case, though
/// it's still present in the JSON (unwired) to keep one JSON builder.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Mutation {
    /// I4: nothing wired to `ramp` — output must not depend on `beat` at all.
    None,
    /// I1 (a): the Sun's `pos_x` — changes `shadow_view_proj` (D6's first
    /// key component).
    LightParam,
    /// I1 (b): the occluder's `transform_3d.pos_x` — changes the model
    /// matrix bytes D6 hashes per draw.
    Transform,
    /// I1 (c): the occluder mesh's `size_x` — a source-PARAM content
    /// change with the SAME vertex count (D6's `vcount` component alone
    /// would miss this; only the generation number catches it).
    MeshContent,
}

/// Ground plane (static, never touched by any mutation) + occluder plane
/// lit by one shadow-casting Sun, mirroring `render_scene_shadows.rs`'s
/// `shadow_scene_json` fixture. `node.beat_ramp` (`rate=1.0, attack=1.0`,
/// so `out == fract(beat)` for `0.0 <= beat < 1.0`) is always present;
/// `mutation` decides which single param it port-shadows, if any.
fn scene_json(mutation: Mutation) -> String {
    let sun_pos_x = if mutation == Mutation::LightParam {
        r#""fromNode":8,"fromPort":"out","toNode":30,"toPort":"pos_x""#.to_string()
    } else {
        String::new()
    };
    let occ_pos_x = if mutation == Mutation::Transform {
        r#""fromNode":8,"fromPort":"out","toNode":7,"toPort":"pos_x""#.to_string()
    } else {
        String::new()
    };
    let occ_size_x = if mutation == Mutation::MeshContent {
        r#""fromNode":8,"fromPort":"out","toNode":5,"toPort":"size_x""#.to_string()
    } else {
        String::new()
    };
    let mut extra_wires = String::new();
    for w in [sun_pos_x, occ_pos_x, occ_size_x] {
        if !w.is_empty() {
            extra_wires.push(',');
            extra_wires.push('{');
            extra_wires.push_str(&w);
            extra_wires.push('}');
        }
    }

    format!(
        r#"{{"version":2,"name":"RenderSceneShadowCacheProof","nodes":[
        {{"id":0,"typeId":"system.generator_input","nodeId":"input"}},
        {{"id":1,"typeId":"node.grid_mesh","nodeId":"ground_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":20}},
            "resolution_y":{{"type":"Int","value":20}},
            "size_x":{{"type":"Float","value":8.0}},
            "size_y":{{"type":"Float","value":8.0}}}}}},
        {{"id":2,"typeId":"node.make_triangles","nodeId":"ground_tris","params":{{
            "src_cols":{{"type":"Int","value":20}},
            "src_rows":{{"type":"Int","value":20}}}}}},
        {{"id":5,"typeId":"node.grid_mesh","nodeId":"occ_grid","params":{{
            "max_capacity":{{"type":"Int","value":8192}},
            "resolution_x":{{"type":"Int","value":10}},
            "resolution_y":{{"type":"Int","value":10}},
            "size_x":{{"type":"Float","value":3.0}},
            "size_y":{{"type":"Float","value":3.0}}}}}},
        {{"id":6,"typeId":"node.make_triangles","nodeId":"occ_tris","params":{{
            "src_cols":{{"type":"Int","value":10}},
            "src_rows":{{"type":"Int","value":10}}}}}},
        {{"id":7,"typeId":"node.transform_3d","nodeId":"occ_xform","params":{{
            "pos_y":{{"type":"Float","value":1.5}}}}}},
        {{"id":3,"typeId":"node.orbit_camera","nodeId":"cam","params":{{
            "orbit":{{"type":"Float","value":0.7}},
            "tilt":{{"type":"Float","value":0.95}},
            "distance":{{"type":"Float","value":10.0}},
            "fov_y":{{"type":"Float","value":0.8}}}}}},
        {{"id":4,"typeId":"node.phong_material","nodeId":"mat","params":{{
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "ambient":{{"type":"Float","value":0.05}}}}}},
        {{"id":8,"typeId":"node.beat_ramp","nodeId":"ramp","params":{{
            "rate":{{"type":"Float","value":1.0}},
            "attack":{{"type":"Float","value":1.0}}}}}},
        {{"id":30,"typeId":"node.light","nodeId":"sun","params":{{
            "mode":{{"type":"Enum","value":0}},
            "pos_x":{{"type":"Float","value":3.0}},
            "pos_y":{{"type":"Float","value":20.0}},
            "pos_z":{{"type":"Float","value":3.0}},
            "aim_x":{{"type":"Float","value":0.0}},
            "aim_y":{{"type":"Float","value":0.0}},
            "aim_z":{{"type":"Float","value":0.0}},
            "color_r":{{"type":"Float","value":1.0}},
            "color_g":{{"type":"Float","value":1.0}},
            "color_b":{{"type":"Float","value":1.0}},
            "intensity":{{"type":"Float","value":1.0}},
            "cast_shadows":{{"type":"Float","value":1.0}}}}}},
        {{"id":20,"typeId":"node.render_scene","nodeId":"scene","params":{{
            "objects":{{"type":"Int","value":2}},
            "lights":{{"type":"Int","value":1}}}}}},
        {{"id":99,"typeId":"system.final_output","nodeId":"out"}}
        ],"wires":[
        {{"fromNode":1,"fromPort":"vertices","toNode":2,"toPort":"in"}},
        {{"fromNode":2,"fromPort":"out","toNode":20,"toPort":"mesh_0"}},
        {{"fromNode":5,"fromPort":"vertices","toNode":6,"toPort":"in"}},
        {{"fromNode":6,"fromPort":"out","toNode":20,"toPort":"mesh_1"}},
        {{"fromNode":7,"fromPort":"transform","toNode":20,"toPort":"transform_1"}},
        {{"fromNode":3,"fromPort":"out","toNode":20,"toPort":"camera"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_0"}},
        {{"fromNode":4,"fromPort":"out","toNode":20,"toPort":"material_1"}},
        {{"fromNode":30,"fromPort":"out","toNode":20,"toPort":"light_0"}},
        {{"fromNode":20,"fromPort":"color","toNode":99,"toPort":"in"}}
        {extra_wires}
        ]}}"#
    )
}

/// One render call at a given `beat`/`frame_count` into `target`, returning
/// the `Rgba16Float` readback bytes. Reuses the SAME `runtime` across calls
/// — the shadow cache's continuity (and the `RenderScene` node instance
/// it lives on) depends on this exactly like `gbuffer_velocity.rs`'s
/// `render_at_beat`.
fn render_at_beat(
    h: &harness::ParityHarness,
    runtime: &mut PresetRuntime,
    target: &manifold_gpu::GpuTexture,
    beat: f64,
    frame_count: i64,
) -> Vec<u8> {
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
    let mut enc = h.device.create_encoder("render-scene-shadow-cache-enc");
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, &h.device);
        runtime.render(&mut gpu, target, &ctx, &manifold_core::params::ParamManifest::default());
    }
    enc.commit_and_wait_completed();
    h.readback(target)
}

fn build_runtime(h: &harness::ParityHarness, json: &str) -> PresetRuntime {
    let registry = PrimitiveRegistry::with_builtin();
    PresetRuntime::from_json_str_with_device(
        json,
        &registry,
        std::sync::Arc::clone(&h.device),
        h.width,
        h.height,
        GpuTextureFormat::Rgba16Float,
        None,
    )
    .unwrap_or_else(|e| panic!("shadow-cache scene graph must build: {e}\n{json}"))
}

/// I4: a static scene's frame 30 (long-running executor, shadow cache hit
/// on every frame after the first) is bit-identical to a fresh executor's
/// frame 1 (which never touches the cache — `shadow_cache_keys` starts
/// `None`, so it always falls through to a real render).
#[test]
fn static_scene_frame_30_matches_fresh_executor_frame_1() {
    let h = harness::shared();
    let json = scene_json(Mutation::None);

    let mut long_running = build_runtime(h, &json);
    let target_a = h.make_target("shadow-cache-static-long-running");
    let mut frame30 = Vec::new();
    for frame in 0..30 {
        // Fixed `beat` — nothing in this scene reads it (no ramp wired),
        // so this is purely "run the same static scene 30 times".
        frame30 = render_at_beat(h, &mut long_running, &target_a.texture, 0.5, frame);
    }

    let mut fresh = build_runtime(h, &json);
    let target_b = h.make_target("shadow-cache-static-fresh");
    let frame1 = render_at_beat(h, &mut fresh, &target_b.texture, 0.5, 0);

    assert_eq!(
        frame30, frame1,
        "I4: a static scene's frame 30 (shadow served from cache since \
         frame 2) must be BIT-IDENTICAL to a fresh executor's frame 1 \
         (cache miss, real render) — any diff means the cache changed \
         what's on screen"
    );
}

/// Shared I1 mutation-trio body: warm the cache at `beat0` for
/// `warm_frames`, mutate to `beat1` on the SAME runtime, and assert that
/// frame's output equals a FRESH executor rendering `beat1` cold.
fn assert_mutation_forces_fresh_equivalent_render(mutation: Mutation, warm_frames: i64) {
    let h = harness::shared();
    let json = scene_json(mutation);

    let mut long_running = build_runtime(h, &json);
    let target_a = h.make_target("shadow-cache-mutation-long-running");
    for frame in 0..warm_frames {
        render_at_beat(h, &mut long_running, &target_a.texture, 0.0, frame);
    }
    let mutated =
        render_at_beat(h, &mut long_running, &target_a.texture, 0.3, warm_frames);

    let mut fresh = build_runtime(h, &json);
    let target_b = h.make_target("shadow-cache-mutation-fresh");
    let fresh_mutated = render_at_beat(h, &mut fresh, &target_b.texture, 0.3, 0);

    assert_eq!(
        mutated, fresh_mutated,
        "I1: a mutated frame on a long-running (cache-warmed) executor must \
         be BIT-IDENTICAL to a fresh executor rendering the same mutated \
         state cold — a diff here means the cache served stale content"
    );
}

/// I1 (a): a light param change (`sun.pos_x`, driving `shadow_view_proj`)
/// forces a real shadow re-render on the very next frame.
#[test]
fn light_param_mutation_matches_fresh_render() {
    assert_mutation_forces_fresh_equivalent_render(Mutation::LightParam, 5);
}

/// I1 (b): an object transform change (`occ_xform.pos_x`, changing the
/// model matrix bytes D6 hashes per draw) forces a real shadow re-render.
#[test]
fn transform_mutation_matches_fresh_render() {
    assert_mutation_forces_fresh_equivalent_render(Mutation::Transform, 5);
}

/// I1 (c): a mesh CONTENT change via a source param (`occ_grid.size_x`) —
/// same vertex count, different vertex data — forces a real shadow
/// re-render. `vcount` alone (a D6 key component) can't catch this; only
/// the vertices-slot generation does.
#[test]
fn mesh_content_mutation_matches_fresh_render() {
    assert_mutation_forces_fresh_equivalent_render(Mutation::MeshContent, 5);
}
