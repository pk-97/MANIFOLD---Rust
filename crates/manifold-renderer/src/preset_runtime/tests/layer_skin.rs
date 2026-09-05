//! SCENE_FX P4a proofs (section 3.3 + section 4 invariants): `node.layer_source`
//! reading the compositor's `LayerSkinRegistry`.
//!
//! Three proofs, matching the design's enforcement table:
//! - `layer` param survives a serde save/reload round trip (the
//!   "layer id survives save/reload" invariant).
//! - Two mutually-skinning layers render 300 frames without hang or panic
//!   (the "feedback is always one-frame" invariant — the registry write
//!   happens after all layer renders, so this loop cannot deadlock).
//! - A scene skinned by a bright source layer tracks that layer's content,
//!   and a missing source id falls back to transparent black without
//!   panicking or clearing the stored param (the "layer_source never
//!   blocks render" invariant). This is also the demo: it dumps PNGs to
//!   /tmp/p4a_layer_skin_demo/ and prints the probe numbers.

#[cfg(feature = "gpu-proofs")]
use manifold_core::BlendMode;
use manifold_core::effect_graph_def::EffectGraphDef;

#[cfg(feature = "gpu-proofs")]
use crate::compositor::{CompositeLayerDescriptor, Compositor, CompositorFrame};
#[cfg(feature = "gpu-proofs")]
use crate::gpu_encoder::GpuEncoder;
#[cfg(feature = "gpu-proofs")]
use crate::layer_compositor::{CompositeClipDescriptor, LayerCompositor};
#[cfg(feature = "gpu-proofs")]
use crate::preset_context::PresetContext;
use crate::preset_runtime::PresetRuntime;
#[cfg(feature = "gpu-proofs")]
use crate::render_target::RenderTarget;
#[cfg(feature = "gpu-proofs")]
use crate::tonemap::TonemapSettings;

#[cfg(feature = "gpu-proofs")]
const W: u32 = 320;
#[cfg(feature = "gpu-proofs")]
const H: u32 = 180;
const LAYER_A: &str = "p4a-layer-a";
#[cfg(feature = "gpu-proofs")]
const LAYER_B: &str = "p4a-layer-b";

fn glb_path() -> String {
    // cubicspline_interp: static (no skin), carries TEXCOORD_0 — an
    // emissive_map needs UVs to sample with (two_material_pbr has none;
    // its materials silently drop the map).
    format!(
        "{}/../../tests/fixtures/gltf/hostile/cubicspline_interp.glb",
        env!("CARGO_MANIFEST_DIR")
    )
}

/// Layer A as a pure skin pass-through: emits layer B's previous frame.
/// The mutual-skin loop is A(t) = B(t-1), B(t) = scene whose emissive map
/// is A(t-1) — the design's legal one-frame feedback.
#[cfg(feature = "gpu-proofs")]
fn pass_through_json(layer_param: &str) -> String {
    format!(
        r#"{{
  "version": 2,
  "name": "PassThroughSkin",
  "nodes": [
    {{ "id": 0, "nodeId": "input", "typeId": "system.generator_input", "handle": "input" }},
    {{ "id": 1, "nodeId": "skin", "typeId": "node.layer_source", "handle": "skin",
       "params": {{ "layer": {{ "type": "String", "value": "{layer}" }} }} }},
    {{ "id": 2, "nodeId": "final_output", "typeId": "system.final_output", "handle": "final_output" }}
  ],
  "wires": [
    {{ "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }}
  ]
}}"#,
        layer = layer_param
    )
}

/// Layer A as a bright 2D generator: a checkerboard at `scale`.
#[cfg(feature = "gpu-proofs")]
fn checkerboard_json(scale: f32) -> String {
    format!(
        r#"{{
  "version": 2,
  "name": "CheckerSource",
  "nodes": [
    {{ "id": 0, "nodeId": "input", "typeId": "system.generator_input", "handle": "input" }},
    {{ "id": 1, "nodeId": "checker", "typeId": "node.checkerboard", "handle": "checker",
       "params": {{ "scale": {{ "type": "Float", "value": {scale} }} }} }},
    {{ "id": 2, "nodeId": "final_output", "typeId": "system.final_output", "handle": "final_output" }}
  ],
  "wires": [
    {{ "fromNode": 1, "fromPort": "out", "toNode": 2, "toPort": "in" }}
  ]
}}"#
    )
}

/// Layer B: glb mesh → scene_object with `node.layer_source` wired into
/// `emissive_map` → render_scene. The P0 spike graph with the in-graph
/// texture atom swapped for the cross-layer skin.
fn scene_skin_json(layer_param: &str) -> String {
    format!(
        r#"{{
  "version": 2,
  "name": "SkinnedScene",
  "nodes": [
    {{ "id": 0, "nodeId": "input", "typeId": "system.generator_input", "handle": "input" }},
    {{ "id": 1, "nodeId": "cam", "typeId": "node.orbit_camera", "handle": "cam",
       "params": {{ "distance": {{ "type": "Float", "value": 2.5 }}, "fov_y": {{ "type": "Float", "value": 0.9 }},
                   "look_y": {{ "type": "Float", "value": 0.5 }}, "orbit": {{ "type": "Float", "value": 0.7 }},
                   "tilt": {{ "type": "Float", "value": 0.35 }} }} }},
    {{ "id": 2, "nodeId": "mesh", "typeId": "node.gltf_mesh_source", "handle": "mesh",
       "params": {{ "path": {{ "type": "String", "value": "{glb}" }},
                   "fit": {{ "type": "Enum", "value": 1 }}, "recenter": {{ "type": "Bool", "value": true }},
                   "material_index": {{ "type": "Int", "value": -1 }}, "mesh_index": {{ "type": "Int", "value": -1 }},
                   "primitive_index": {{ "type": "Int", "value": -1 }},
                   "max_capacity": {{ "type": "Int", "value": 100000 }} }} }},
    {{ "id": 3, "nodeId": "mat", "typeId": "node.phong_material", "handle": "mat",
       "params": {{ "color_r": {{ "type": "Float", "value": 0.9 }}, "color_g": {{ "type": "Float", "value": 0.9 }},
                   "color_b": {{ "type": "Float", "value": 0.9 }}, "ambient": {{ "type": "Float", "value": 0.4 }},
                   "emission_r": {{ "type": "Float", "value": 1.0 }}, "emission_g": {{ "type": "Float", "value": 1.0 }},
                   "emission_b": {{ "type": "Float", "value": 1.0 }},
                   "emission_intensity": {{ "type": "Float", "value": 2.0 }} }} }},
    {{ "id": 4, "nodeId": "xform", "typeId": "node.transform_3d", "handle": "xform",
       "params": {{ "pos_y": {{ "type": "Float", "value": 0.0 }} }} }},
    {{ "id": 5, "nodeId": "skin", "typeId": "node.layer_source", "handle": "skin",
       "params": {{ "layer": {{ "type": "String", "value": "{layer}" }} }} }},
    {{ "id": 6, "nodeId": "sun", "typeId": "node.light", "handle": "sun",
       "params": {{ "mode": {{ "type": "Enum", "value": 0 }},
                   "pos_x": {{ "type": "Float", "value": 0.0 }}, "pos_y": {{ "type": "Float", "value": 5.0 }},
                   "pos_z": {{ "type": "Float", "value": 5.0 }},
                   "aim_x": {{ "type": "Float", "value": 0.0 }}, "aim_y": {{ "type": "Float", "value": 0.0 }},
                   "aim_z": {{ "type": "Float", "value": 0.0 }},
                   "color_r": {{ "type": "Float", "value": 1.0 }}, "color_g": {{ "type": "Float", "value": 1.0 }},
                   "color_b": {{ "type": "Float", "value": 1.0 }}, "intensity": {{ "type": "Float", "value": 1.0 }} }} }},
    {{ "id": 7, "nodeId": "object", "typeId": "node.scene_object", "handle": "object" }},
    {{ "id": 8, "nodeId": "scene", "typeId": "node.render_scene", "handle": "scene",
       "params": {{ "objects": {{ "type": "Int", "value": 1 }}, "lights": {{ "type": "Int", "value": 1 }} }} }},
    {{ "id": 9, "nodeId": "final_output", "typeId": "system.final_output", "handle": "final_output" }}
  ],
  "wires": [
    {{ "fromNode": 1, "fromPort": "out", "toNode": 8, "toPort": "camera" }},
    {{ "fromNode": 2, "fromPort": "vertices", "toNode": 7, "toPort": "vertices" }},
    {{ "fromNode": 3, "fromPort": "out", "toNode": 7, "toPort": "material" }},
    {{ "fromNode": 4, "fromPort": "transform", "toNode": 7, "toPort": "transform" }},
    {{ "fromNode": 5, "fromPort": "out", "toNode": 7, "toPort": "emissive_map" }},
    {{ "fromNode": 6, "fromPort": "out", "toNode": 8, "toPort": "light_0" }},
    {{ "fromNode": 7, "fromPort": "object", "toNode": 8, "toPort": "object_0" }},
    {{ "fromNode": 8, "fromPort": "color", "toNode": 9, "toPort": "in" }}
  ]
}}"#,
        glb = glb_path(),
        layer = layer_param
    )
}

/// The `layer` param is def-baked on the node (BUG-182's def-value path,
/// same as the glb importer's mesh sources) — it must survive a
/// save → reload serde round trip untouched, and the reloaded def must
/// still build a runtime.
#[test]
fn layer_param_survives_serde_round_trip() {
    let json = scene_skin_json(LAYER_A);

    let saved: EffectGraphDef =
        serde_json::from_str(&json).expect("scene_skin_json must parse");
    let reserialized = serde_json::to_string(&saved).expect("def must serialize");
    let reloaded: EffectGraphDef =
        serde_json::from_str(&reserialized).expect("round-tripped def must re-parse");

    let skin_node = reloaded
        .nodes
        .iter()
        .find(|n| n.type_id == "node.layer_source")
        .expect("layer_source node survives the round trip");
    match skin_node
        .params
        .get("layer")
        .expect("layer param survives the round trip")
    {
        manifold_core::effect_graph_def::SerializedParamValue::String { value } => {
            assert_eq!(value, LAYER_A, "layer id must round-trip intact");
        }
        other => panic!("layer param must stay String-typed, got {other:?}"),
    }

    // The reloaded def must still build (the "reload" half of the invariant).
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    PresetRuntime::from_json_str(&reserialized, &registry)
        .expect("round-tripped def must build a runtime");
}

/// One frame of the harness: render both layer graphs into their clip
/// textures, then composite — the compositor's end-of-frame publish is
/// what layer_source reads NEXT frame. Mirrors the content-pipeline order
/// (generators commit before the compositor).
#[cfg(feature = "gpu-proofs")]
#[allow(clippy::too_many_arguments)]
fn render_two_layer_frame(
    device: &manifold_gpu::GpuDevice,
    compositor: &mut LayerCompositor,
    runtime_a: Option<&mut PresetRuntime>,
    runtime_b: &mut PresetRuntime,
    target_a: &RenderTarget,
    target_b: &RenderTarget,
    layer_a_id: &manifold_core::LayerId,
    layer_b_id: &manifold_core::LayerId,
    frame: u64,
) {
    let time = frame as f64 / 60.0;
    let dt = 1.0 / 60.0;
    let ctx = |width: u32, height: u32| PresetContext {
        time,
        beat: time * 2.0,
        dt,
        width,
        height,
        output_width: width,
        output_height: height,
        aspect: width as f32 / height as f32,
        owner_key: 0,
        is_clip_level: false,
        frame_count: frame as i64,
        anim_progress: 0.0,
        trigger_count: 0,
        gpu_signal_committed: 0,
        gpu_signaled: 0,
    };

    // Generators first, one committed encoder.
    let mut enc = device.create_encoder("layer-skin-frame");
    {
        let mut gpu = GpuEncoder::new(&mut enc, device);
        if let Some(a) = runtime_a {
            a.render(
                &mut gpu,
                &target_a.texture,
                &ctx(W, H),
                &manifold_core::params::ParamManifest::default(),
            );
        } else {
            gpu.clear_texture(&target_a.texture, 0.0, 0.0, 0.0, 0.0);
        }
        runtime_b.render(
            &mut gpu,
            &target_b.texture,
            &ctx(W, H),
            &manifold_core::params::ParamManifest::default(),
        );
    }
    enc.commit();

    // Then the compositor (its end-of-frame publish fills the registry).
    let mut enc = device.create_encoder("layer-skin-composite");
    {
        let mut gpu = GpuEncoder::new(&mut enc, device);

        // Layer 0 is the top of the stack: the skinned scene (B) sits on
        // top, the source layer (A) shows through behind it.
        let clip_a = CompositeClipDescriptor {
            clip_id: "clip-a",
            texture: &target_a.texture,
            layer_index: 1,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            is_muted: false,
            effects: &[],
            effect_groups: &[],
        };
        let clip_b = CompositeClipDescriptor {
            clip_id: "clip-b",
            texture: &target_b.texture,
            layer_index: 0,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            is_muted: false,
            effects: &[],
            effect_groups: &[],
        };
        // generate_layers expects clips grouped by descending layer_index:
        // [A(1), B(0)] — B (the scene, layer 0 = top of the stack) blends
        // last, over the source layer showing through behind it.
        let clips = [clip_a, clip_b];
        let layer_a = CompositeLayerDescriptor {
            layer_index: 1,
            layer_id: layer_a_id,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            hidden: false,
            blit_to_led: false,
            layer_type: manifold_core::LayerType::Video,
            effects: &[],
            effect_groups: &[],
            parent_layer_id: None,
            is_group: false,
            trigger_count: 0,
        };
        let layer_b = CompositeLayerDescriptor {
            layer_index: 0,
            layer_id: layer_b_id,
            blend_mode: BlendMode::Normal,
            opacity: 1.0,
            hidden: false,
            blit_to_led: false,
            layer_type: manifold_core::LayerType::Video,
            effects: &[],
            effect_groups: &[],
            parent_layer_id: None,
            is_group: false,
            trigger_count: 0,
        };
        let layers = [layer_a, layer_b];
        let frame_ctx = CompositorFrame {
            time,
            beat: time * 2.0,
            dt,
            frame_count: frame,
            compositor_dirty: false,
            clips: &clips,
            layers: &layers,
            master_effects: &[],
            master_effect_groups: &[],
            master_trigger_count: 0,
            tonemap: TonemapSettings::default(),
            led_exit_index: -1,
            // (1, 1) like the compositor's own frame test — a 0×0 LED size
            // reaches an unclamped texture allocation.
            led_composite_size: (1, 1),
            output_width: W,
            output_height: H,
            occluded_layers: &[],
            render_skip: &[],
            gpu_signal_committed: 0,
            gpu_signaled: 0,
        };
        compositor.render(&mut gpu, &frame_ctx);
    }
    enc.commit();
}

/// Read back a texture's per-pixel luma over the center box (the model's
/// screen position with the fixture camera), decoded from Rgba16Float.
/// Returns the region pixels; `region_mean`/`mean_abs_delta` derive the
/// probe numbers.
#[cfg(feature = "gpu-proofs")]
fn region_luma_pixels(
    device: &manifold_gpu::GpuDevice,
    texture: &manifold_gpu::GpuTexture,
) -> Vec<f32> {
    let bytes_per_row = W * 8;
    let buf = device.create_buffer_shared(u64::from(H * bytes_per_row));
    let mut rb = device.create_encoder("layer-skin-readback");
    rb.copy_texture_to_buffer(texture, &buf, W, H, bytes_per_row);
    rb.commit_and_wait_completed();
    let ptr = buf.mapped_ptr().expect("shared buffer mapped");
    let px: &[u16] =
        unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (W * H * 4) as usize) };

    let x0 = W * 3 / 8;
    let x1 = W * 5 / 8;
    let y0 = H * 3 / 8;
    let y1 = H * 5 / 8;
    let mut out = Vec::with_capacity(((x1 - x0) * (y1 - y0)) as usize);
    for y in y0..y1 {
        for x in x0..x1 {
            let i = ((y * W + x) * 4) as usize;
            out.push(
                (half::f16::from_bits(px[i]).to_f32()
                    + half::f16::from_bits(px[i + 1]).to_f32()
                    + half::f16::from_bits(px[i + 2]).to_f32())
                    / 3.0,
            );
        }
    }
    out
}

#[cfg(feature = "gpu-proofs")]
fn region_mean(region: &[f32]) -> f32 {
    region.iter().sum::<f32>() / region.len() as f32
}

#[cfg(feature = "gpu-proofs")]
fn mean_abs_delta(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| (x - y).abs()).sum::<f32>() / a.len() as f32
}

/// Two layers skinning each other render 300 frames without hang or panic,
/// and the pass-through layer carries the scene's energy one frame later —
/// the registry publish → next-frame read loop actually closed.
#[cfg(feature = "gpu-proofs")]
#[test]
fn mutual_skin_two_layers_render_300_frames() {
    let device = crate::test_device();
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let format = manifold_gpu::GpuTextureFormat::Rgba16Float;

    let mut runtime_a = PresetRuntime::from_json_str_with_device(
        &pass_through_json(LAYER_B),
        &registry,
        device.arc(),
        W,
        H,
        format,
        None,
    )
    .expect("pass-through graph must load");
    let mut runtime_b = PresetRuntime::from_json_str_with_device(
        &scene_skin_json(LAYER_A),
        &registry,
        device.arc(),
        W,
        H,
        format,
        None,
    )
    .expect("scene graph must load");

    let mut compositor = LayerCompositor::new(&device, W, H);
    let target_a = RenderTarget::new(&device, W, H, format, "layer-skin-a");
    let target_b = RenderTarget::new(&device, W, H, format, "layer-skin-b");
    let layer_a_id = manifold_core::LayerId::new(LAYER_A);
    let layer_b_id = manifold_core::LayerId::new(LAYER_B);

    // The registry pointer is set once — the compositor owns the registry
    // for the whole test, mirroring the content-pipeline wiring.
    {
        // Safety: the compositor (and its registry) outlives both runtimes.
        let ptr = crate::layer_skin::LayerSkinPtr::new(
            compositor.layer_skin_registry().expect("compositor registry"),
        );
        runtime_a.set_layer_skin_registry(Some(unsafe { ptr.get() }));
        runtime_b.set_layer_skin_registry(Some(unsafe { ptr.get() }));
    }

    // Frame-cost measurement: each iteration drains the GPU (an empty
    // encoder's commit_and_wait waits every earlier buffer on the device's
    // single queue), so per-frame wall time is the true two-layer skin
    // frame cost — the MANIFOLD_RENDER_TRACE budget check, measured harder.
    // Frames 0..WARMUP are cold start (GLB parse + first-use pipeline
    // compiles — the accepted render_scene cold-frame pattern; startup
    // prewarm is the app's job, not this harness's) and are reported but
    // not budget-checked; the 20 ms budget is steady state.
    const WARMUP: u64 = 10;
    let mut cold_max_ms = 0.0f64;
    let mut max_frame_ms = 0.0f64;
    let mut total_ms = 0.0f64;
    let mut steady_frames = 0u64;
    for frame in 0..300 {
        let t = std::time::Instant::now();
        render_two_layer_frame(
            &device,
            &mut compositor,
            Some(&mut runtime_a),
            &mut runtime_b,
            &target_a,
            &target_b,
            &layer_a_id,
            &layer_b_id,
            frame,
        );
        device
            .create_encoder("layer-skin-drain")
            .commit_and_wait_completed();
        let ms = t.elapsed().as_secs_f64() * 1000.0;
        if frame < WARMUP {
            cold_max_ms = cold_max_ms.max(ms);
        } else {
            max_frame_ms = max_frame_ms.max(ms);
            total_ms += ms;
            steady_frames += 1;
        }
        if frame >= 295 {
            println!("mutual-skin frame {frame}: {ms:.2} ms");
        }
    }
    println!(
        "mutual-skin frame cost: cold max {cold_max_ms:.2} ms (first {WARMUP} frames), \
         steady max {max_frame_ms:.2} ms, steady avg {:.2} ms over {steady_frames} frames \
         (budget 20 ms)",
        total_ms / steady_frames as f64
    );
    assert!(
        max_frame_ms < 20.0,
        "two-layer mutual-skin steady-state frame cost must stay under 20 ms \
         (max {max_frame_ms:.2} ms)"
    );

    // The loop closed: A is a pure pass-through of B's previous frame, so
    // after 300 steady frames A's pixels must reproduce B's (one frame of
    // lag, near-identical region means) and B must carry visible energy.
    let mean_a = region_mean(&region_luma_pixels(&device, &target_a.texture));
    let mean_b = region_mean(&region_luma_pixels(&device, &target_b.texture));
    println!("mutual-skin probe: A region mean {mean_a:.4}, B region mean {mean_b:.4}");
    assert!(
        mean_b > 0.001,
        "the skinned scene must render visible energy (mean {mean_b})"
    );
    assert!(
        (mean_a - mean_b).abs() / mean_b.max(1e-6) < 0.25,
        "the pass-through layer must reproduce the source layer's content \
         (A {mean_a} vs B {mean_b} — registry publish or layer_source read is dead)"
    );
}

/// The demo proof: a scene's model region tracks the source layer's
/// content, and a missing source id emits the transparent-black fallback.
#[cfg(feature = "gpu-proofs")]
#[test]
fn skin_tracks_source_content_and_missing_id_falls_back() {
    let device = crate::test_device();
    let registry = crate::node_graph::PrimitiveRegistry::with_builtin();
    let format = manifold_gpu::GpuTextureFormat::Rgba16Float;

    let mut runtime_b = PresetRuntime::from_json_str_with_device(
        &scene_skin_json(LAYER_A),
        &registry,
        device.arc(),
        W,
        H,
        format,
        None,
    )
    .expect("scene graph must load");

    let mut compositor = LayerCompositor::new(&device, W, H);
    let target_a = RenderTarget::new(&device, W, H, format, "layer-skin-a");
    let target_b = RenderTarget::new(&device, W, H, format, "layer-skin-b");
    let layer_a_id = manifold_core::LayerId::new(LAYER_A);
    let layer_b_id = manifold_core::LayerId::new(LAYER_B);

    {
        let ptr = crate::layer_skin::LayerSkinPtr::new(
            compositor.layer_skin_registry().expect("compositor registry"),
        );
        runtime_b.set_layer_skin_registry(Some(unsafe { ptr.get() }));
    }

    // One variant of layer A per probe: a bright checkerboard at two
    // scales (content changes between them) and a missing layer (A never
    // renders → registry lookup misses → fallback). Each variant runs
    // long enough for the one-frame delay to propagate (3 frames).
    let mut probe = |checker_scale: Option<f32>, name: &str| -> Vec<f32> {
        let mut runtime_a = checker_scale.map(|scale| {
            PresetRuntime::from_json_str_with_device(
                &checkerboard_json(scale),
                &registry,
                device.arc(),
                W,
                H,
                format,
                None,
            )
            .expect("checkerboard graph must load")
        });
        // Warmup: the GLB parse and accel build land in the first frames;
        // the first variant also warms every pipeline.
        for frame in 0..30 {
            let a_ref = runtime_a.as_mut();
            render_two_layer_frame(
                &device,
                &mut compositor,
                a_ref,
                &mut runtime_b,
                &target_a,
                &target_b,
                &layer_a_id,
                &layer_b_id,
                frame,
            );
        }
        // Demo artifacts: the two-layer composite (checkerboard + skinned
        // scene) AND the skinned scene layer's own texture — the direct
        // look at the model wearing the skin.
        let dir = std::path::Path::new("/tmp/p4a_layer_skin_demo");
        std::fs::create_dir_all(dir).expect("create demo dir");
        let dump_png = |texture: &manifold_gpu::GpuTexture, path: std::path::PathBuf| {
            let bytes_per_row = texture.width * 8;
            let buf = device.create_buffer_shared(u64::from(texture.height * bytes_per_row));
            let mut rb = device.create_encoder("layer-skin-demo-readback");
            rb.copy_texture_to_buffer(texture, &buf, texture.width, texture.height, bytes_per_row);
            rb.commit_and_wait_completed();
            let ptr = buf.mapped_ptr().expect("shared buffer mapped");
            let px: &[u16] = unsafe {
                std::slice::from_raw_parts(
                    ptr.cast::<u16>(),
                    (texture.width * texture.height * 4) as usize,
                )
            };
            let mut rgba8 = Vec::with_capacity(px.len());
            for c in px.chunks(4) {
                for v in c.iter().take(3) {
                    rgba8.push(crate::headless_readback::linear_to_srgb8(
                        half::f16::from_bits(*v).to_f32(),
                    ));
                }
                rgba8.push(255);
            }
            let png = crate::headless_readback::encode_rgba8_png(&rgba8, texture.width, texture.height);
            std::fs::write(&path, png).expect("write demo png");
            println!("demo artifact: {}", path.display());
        };
        dump_png(
            compositor.output_texture(),
            dir.join(format!("composite_{name}.png")),
        );
        dump_png(&target_b.texture, dir.join(format!("model_{name}.png")));

        region_luma_pixels(&device, &target_b.texture)
    };

    let region_missing = probe(None, "fallback_missing_layer");
    let region_scale_8 = probe(Some(8.0), "skin_checker_scale8");
    let region_scale_2 = probe(Some(2.0), "skin_checker_scale2");

    let mean_missing = region_mean(&region_missing);
    let mean_scale_8 = region_mean(&region_scale_8);
    let mean_scale_2 = region_mean(&region_scale_2);
    let max_scale_8 = region_scale_8.iter().copied().fold(0.0f32, f32::max);
    let delta_8_vs_2 = mean_abs_delta(&region_scale_8, &region_scale_2);
    let delta_8_vs_missing = mean_abs_delta(&region_scale_8, &region_missing);

    println!(
        "layer-skin probe: model region — mean missing={mean_missing:.4} \
         scale8={mean_scale_8:.4} scale2={mean_scale_2:.4}; region max scale8 \
         {max_scale_8:.4}; mean|Δ| scale8-vs-scale2 {delta_8_vs_2:.4}, \
         scale8-vs-missing {delta_8_vs_missing:.4}"
    );

    // A checkerboard's average is scale-invariant, so the content-change
    // signal is per-pixel, not in the mean: different cell layouts on the
    // model's UVs must move the region's pixels.
    assert!(
        mean_scale_8 - mean_missing > 0.03,
        "a skinned model must brighten over the fallback \
         (scale8 {mean_scale_8} vs missing {mean_missing})"
    );
    assert!(
        delta_8_vs_2 > 0.05,
        "the skin must track source-content changes per-pixel \
         (mean|Δ| scale8-vs-scale2 {delta_8_vs_2})"
    );
    assert!(
        mean_missing < mean_scale_8,
        "missing id must fall back to the darker, diffuse-only look"
    );
}
