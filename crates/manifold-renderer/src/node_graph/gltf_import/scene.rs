//! Full-scene assembly: from a parsed [`GltfImportSummary`] build the whole
//! generator graph — every object group, the shared render_scene, the
//! framing camera, sun/fill/strip lights, the IBL envmap and the outer
//! performance card surface.

use std::path::Path;

use manifold_core::NodeId;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire,
    GROUP_INPUT_TYPE_ID, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface,
    InterfacePortDef, ParamSpecDef, PresetMetadata, SkipModeDef, StringBindingDef,
    StringParamSpecDef,
};
use manifold_core::scene_exposure::stamp_scene_node_exposures_into;

use crate::node_graph::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID};
use crate::node_graph::gltf_load;
use crate::node_graph::gltf_load::GltfImportSummary;
use crate::node_graph::primitives::DEFAULT_NEAR as CAMERA_NEAR_DEFAULT;
use crate::node_graph::primitives::render_scene::OBJECT_SAFETY_MAX;
use crate::node_graph::scene_exposure::metadata_for_node_type;

use super::ImportReport;
use super::{HDRI_FILE_PARAM_ID, MODEL_FILE_PARAM_ID};
use super::assembly::*;
use super::cards::*;
use super::object_group::*;

/// Default softbox dome-fill radiance stamped on imported rigs (F-P7) —
/// node param and Fill Light card default stay in sync through this one
/// constant. Tuned against the DamagedHelmet/AMG probe renders: enough
/// broad radiance that metallic surfaces read their albedo, low enough
/// that the black-void product look survives.
pub(super) const IMPORT_FILL_DEFAULT: f32 = 0.6;

/// Default softbox strip intensity stamped on imported rigs (F-P7): half
/// the primitive's own 6.0 default. With the fill dome supplying the broad
/// radiance, full-strength strips dominate every curved reflection (the
/// banded-visor look); 3.0 keeps the chrome-streak character as an accent.
/// The Strip Lights card fader dials it live.
pub(super) const IMPORT_STRIPS_DEFAULT: f32 = 3.0;

/// Assemble the generator graph from an already-parsed [`GltfImportSummary`].
/// Split from [`assemble_import_graph`] (which owns the single file parse) so the
/// graph shape — including the per-object node **grouping** — is testable against
/// a synthetic summary with no `.glb` fixture on disk.
///
/// Each distinct material becomes one node **group** (`GROUP_TYPE_ID`) named for
/// the material: its `node.gltf_mesh_source` + `node.pbr_material` +
/// `node.transform_3d` (+ optional `node.gltf_texture_source`) live inside,
/// bound by an inner `node.scene_object` and exposed through a
/// `system.group_output` as a single `object: Object` port, wired to the
/// shared `node.render_scene`'s `object_{k}` port (SCENE_OBJECT_AND_PANEL_V2_DESIGN
/// D1/D3/D4). Grouping is a pure presentation layer: it flattens
/// away at load (`manifold_core::flatten::flatten_groups`, run inside
/// `instantiate_def`) to the exact same flat graph, and every inner node keeps its
/// stable `node_id`, so the card/string bindings that target `mesh_k`/`mat_k`/
/// `tex_k`/`transform_k` by id resolve unchanged (see `docs/GROUPING_GRAPHS.md` §2).
pub(super) fn build_import_graph(
    summary: &GltfImportSummary,
    path: &Path,
) -> Result<(EffectGraphDef, ImportReport), String> {
    if summary.materials.is_empty() {
        return Err(format!(
            "{}: no materials with geometry — nothing to import",
            path.display()
        ));
    }

    // GLB_CONFORMANCE_DESIGN.md D4: import is 1:1 — every material with
    // geometry gets its own render_scene object, never a truncated prefix.
    // `OBJECT_SAFETY_MAX` (1024) is a real GPU/port-list safety bound, not a
    // curation cap: an asset beyond it errors loudly instead of silently
    // dropping geometry.
    if summary.materials.len() > OBJECT_SAFETY_MAX as usize {
        return Err(format!(
            "{}: {} materials with geometry exceeds the {}-object safety bound — \
             this asset cannot be imported 1:1 without risking a runaway port-list \
             (raise OBJECT_SAFETY_MAX in render_scene.rs if a real asset legitimately \
             needs more; never silently truncate)",
            path.display(),
            summary.materials.len(),
            OBJECT_SAFETY_MAX,
        ));
    }
    // Largest-by-vertex-count first: historical ordering; P1 exposes every
    // material's params, so this is no longer a curation boundary.
    let mut materials = summary.materials.clone();
    materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
    let n = materials.len();
    if summary.default_material_vertex_count > 0 {
        // This log line is informational, not a
        // warning about dropped data — kept at `warn!` so it's still easy
        // to spot in a log dump when triaging an import.
        log::warn!(
            "gltf_import::assemble_import_graph({}): {} vertices belong to glTF's unassigned \
             default material — imported as a normal object (glTF spec §3.9.2 implicit default)",
            path.display(),
            summary.default_material_vertex_count,
        );
    }
    if summary.camera_count > 0 {
        log::info!(
            "gltf_import::assemble_import_graph({}): glb carries {} embedded camera(s) — v1 \
             ignores them and synthesizes its own bbox-framed orbit camera",
            path.display(),
            summary.camera_count,
        );
    }

    let center = [
        (summary.bbox_min[0] + summary.bbox_max[0]) * 0.5,
        (summary.bbox_min[1] + summary.bbox_max[1]) * 0.5,
        (summary.bbox_min[2] + summary.bbox_max[2]) * 0.5,
    ];
    let dims = [
        summary.bbox_max[0] - summary.bbox_min[0],
        summary.bbox_max[1] - summary.bbox_min[1],
        summary.bbox_max[2] - summary.bbox_min[2],
    ];
    let radius =
        ((dims[0] * dims[0] + dims[1] * dims[1] + dims[2] * dims[2]).sqrt() * 0.5).max(1e-3);
    // Camera vertical FOV — hoisted here so both the framing-distance fit
    // below and the camera node's own `fov_y` param (set further down) read
    // the SAME value; a duplicated literal is how BUG-206-style drift
    // happens in the first place.
    let fov_y = 0.9_f32;
    // `2.2 * radius` alone frames by the bbox half-DIAGONAL,
    // which for an elongated (tall/thin) object barely exceeds its dominant
    // axis — the frame's vertical span contains the object with almost no
    // margin, and camera tilt + perspective push it past the top/bottom
    // edges. Frame by PER-AXIS fit instead: for each axis, the distance
    // required so that half the axis's extent subtends no more than the
    // half-FOV, with a 1.15 safety margin. The render aspect isn't known at
    // import time, so the horizontal half-angle is conservatively treated
    // as equal to the vertical one (square-aspect assumption — never
    // UNDER-frames a wider-than-tall render).
    let half_fov_tan = (fov_y * 0.5).tan();
    let per_axis_fit = dims
        .iter()
        .map(|&extent| (extent * 0.5) / half_fov_tan * 1.15)
        .fold(0.0f32, f32::max);
    // The `2.2 * radius` floor keeps every COMPACT asset's framing IDENTICAL
    // to before this fix (the golden-stability guarantee: per_axis_fit is
    // only ever larger than the floor for objects dominated by one axis,
    // where the diagonal-based distance genuinely under-frames).
    let distance = (2.2 * radius).max(per_axis_fit);
    // BUG-165/BUG-169 root cause (diagnosed via GLB_XFAIL_BURNDOWN_DESIGN.md
    // P1's `--trace` instrument): `node.orbit_camera`'s `near` clip plane
    // defaults to a fixed 0.05 (camera_orbit.rs), which was never scaled to
    // the framed object's own size. `distance = 2.2 * radius` already
    // scales with the object, so the object's front face sits at
    // `distance - radius == 1.2 * radius` from the camera — for any object
    // with `radius` below ~0.042 (BoomBox: 0.0172, MetalRoughSpheresNoTextures:
    // 0.0056 — both real-world-scale Khronos assets authored in meters),
    // the fixed near plane sits IN FRONT of the object and the whole frame
    // clips to black every frame.
    //
    // Fix: `near` tracks the object's own front-face distance (with a 2x
    // safety margin so the surface never grazes the plane), capped at the
    // pre-existing 0.05 default so every currently-passing asset whose
    // front face already clears 0.05 gets the IDENTICAL near value as
    // before (front_margin = 1.2 * radius stays >= 0.05 whenever radius >=
    // ~0.0417 — true for every other passing Khronos asset checked:
    // WaterBottle radius 0.151, DamagedHelmet 1.64, MetalRoughSpheres 6.99,
    // TextureSettingsTest 7.21, Duck 1.27, Box 0.87). Only genuinely
    // sub-threshold objects get a smaller near plane.
    let front_margin = (distance - radius).max(1e-4);
    let near_clip = CAMERA_NEAR_DEFAULT.min(front_margin * 0.5);

    let path_str = path.to_string_lossy().into_owned();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ImportedModel".to_string());
    let sanitized = sanitize_identifier(&stem);
    let osc_prefix = sanitized.to_lowercase();

    let mut nodes = Vec::new();
    let mut wires = Vec::new();
    // P1 scene-panel exposure convergence: every scene-vocabulary atom's params
    // become outer-card sliders. These vectors accumulate across the import and
    // attach to `preset_metadata` at the end.
    let mut card_params: Vec<ParamSpecDef> = Vec::new();
    let mut card_bindings: Vec<BindingDef> = Vec::new();
    let mut next_id = 0u32;
    let mut fresh_id = move || {
        let v = next_id;
        next_id += 1;
        v
    };

    let input_id = fresh_id();
    nodes.push(plain_node(input_id, "input", GENERATOR_INPUT_TYPE_ID, "input"));

    // IMPORT_FIDELITY_DESIGN.md D7 (F-P4) — the default import look is the
    // "black-void studio": `mode = softbox` (exact-zero black base + bright
    // emitter strips, never the legacy gradient studio) at `intensity = 1.0`
    // (the F-P1/F-P3 default already matches `mode`'s own primitive
    // defaults — emitter_count/intensity/elevation/width — so nothing else
    // needs stamping here). Superseded the old "intensity 0, lights-only"
    // default; the Environment card below (`env_intensity`) now starts at
    // 1.0 to match.
    let envmap_id = fresh_id();
    let mut envmap_node = plain_node(envmap_id, "envmap", "node.bake_environment", "envmap");
    envmap_node.params.insert("intensity".to_string(), float(1.0));
    envmap_node.params.insert("mode".to_string(), enum_val(1)); // Softbox
    // F-P7 dome fill: metals are lit exclusively by the environment, so
    // against D7's pure-black void every metallic import read as dark
    // chrome regardless of its albedo (the 2026-07-15 helmet/AMG failure).
    // A modest neutral dome gives metals a world to reflect while the
    // background stays black (the envmap is never drawn as a backdrop).
    // The Fill Light card slider below dials it, 0 = the original void.
    envmap_node.params.insert("fill".to_string(), float(IMPORT_FILL_DEFAULT));
    envmap_node
        .params
        .insert("emitter_intensity".to_string(), float(IMPORT_STRIPS_DEFAULT));
    let envmap_node_params = envmap_node.params.clone();
    nodes.push(envmap_node);
    // P1: expose every scene-vocabulary atom's params from the primitive registry.
    // Fan-out bindings that link a slider to more than one target are added below
    // using the helper-generated ids.
    stamp_scene_node_exposures_into(
        &mut card_params,
        &mut card_bindings,
        envmap_id,
        &NodeId::new("envmap"),
        "node.bake_environment",
        "Environment",
        &metadata_for_node_type("node.bake_environment"),
        &envmap_node_params,
    );

    // GLB_CONFORMANCE_DESIGN.md D6 — `node.hdri_source` decodes a real-world
    // linear-HDR .exr (Browse-wired via the `hdri_file` string binding
    // below) and `node.switch_texture` picks between it and the softbox
    // bake above by the `env_mode` card enum (default 0 = Softbox, so the
    // black-void aesthetic stays the import default — Peter, 2026-07-15:
    // "I quite like the pure void and sunlight only look"). `render_scene`'s
    // `envmap` input now wires from the switch's `out`, not the bake
    // directly.
    let hdri_id = fresh_id();
    nodes.push(plain_node(hdri_id, "hdri", "node.hdri_source", "hdri"));

    // HDRI exposure stage: `node.bake_environment` has its own `intensity`
    // master (the Environment card fader's original target), but a decoded
    // EXR arrives at the file's true radiance — a real daytime pure-sky
    // HDRI averages ~0.2–0.4 linear (measured on kloppenheim_07: mean 0.24,
    // sky half 0.34), roughly 4× dimmer than the softbox default. Without
    // an exposure stage the Environment fader would be dead in HDRI mode
    // and the performer would have no way to bring a real sky up to stage
    // brightness. `node.gain` on the HDRI branch (range 0–4, matching the
    // card fader) restores symmetry: env_intensity fans out to BOTH
    // envmap.intensity and this gain, so one fader is the environment
    // master in either mode — same fan-out pattern as the sun_x/y/z macros.
    // (`node.exposure` is the gain atom's type id; its param is `gain`.)
    let hdri_gain_id = fresh_id();
    nodes.push(plain_node(hdri_gain_id, "hdri_gain", "node.exposure", "hdri_gain"));

    let env_select_id = fresh_id();
    let mut env_select_node =
        plain_node(env_select_id, "env_select", "node.switch_texture", "env_select");
    env_select_node.params.insert("num_inputs".to_string(), int(2));
    env_select_node.params.insert("selector".to_string(), float(0.0)); // 0 = Softbox
    nodes.push(env_select_node);

    let camera_id = fresh_id();
    let mut cam_node = plain_node(camera_id, "camera", "node.orbit_camera", "camera");
    cam_node.params.insert("orbit".to_string(), float(0.7));
    cam_node.params.insert("tilt".to_string(), float(0.3));
    cam_node.params.insert("distance".to_string(), float(distance));
    cam_node.params.insert("fov_y".to_string(), float(fov_y));
    cam_node.params.insert("look_y".to_string(), float(0.0));
    // BUG-165/BUG-169 fix — see `near_clip` computation above.
    cam_node.params.insert("near".to_string(), float(near_clip));
    let cam_node_params = cam_node.params.clone();
    nodes.push(cam_node);
    stamp_scene_node_exposures_into(
        &mut card_params,
        &mut card_bindings,
        camera_id,
        &NodeId::new("camera"),
        "node.orbit_camera",
        "Camera",
        &metadata_for_node_type("node.orbit_camera"),
        &cam_node_params,
    );
    // Orbit and tilt are full 360 rotations, not clamped ranges (Peter,
    // 2026-07-15). The primitive ParamDef does not carry `wraps`, so we
    // override the auto-exposed spec after stamping.
    for param in ["orbit", "tilt"] {
        let id = format!("{camera_id}_{param}");
        if let Some(p) = card_params.iter_mut().find(|p| p.id == id) {
            p.wraps = true;
        }
    }

    // Physical lens (CINEMATIC_POST D6): sits between the raw orbit camera
    // and render_scene/ao. No depth-of-field consumer wired anymore
    // and no motion_blur consumer either (see the motion_blur removal note below),
    // so `shutter_angle`/`focus_distance`/`f_stop` are along for the ride
    // only insofar as `node.camera_lens` requires them — nothing downstream
    // reads them today.
    let lens_id = fresh_id();
    let mut lens_node = plain_node(lens_id, "lens", "node.camera_lens", "lens");
    lens_node.params.insert("focus_distance".to_string(), float(distance));
    lens_node.params.insert("f_stop".to_string(), float(32.0));
    let lens_node_params = lens_node.params.clone();
    nodes.push(lens_node);
    stamp_scene_node_exposures_into(
        &mut card_params,
        &mut card_bindings,
        lens_id,
        &NodeId::new("lens"),
        "node.camera_lens",
        "Camera",
        &metadata_for_node_type("node.camera_lens"),
        &lens_node_params,
    );

    let sun_id = fresh_id();
    let mut sun_node = plain_node(sun_id, "sun", "node.light", "sun");
    sun_node.params.insert("mode".to_string(), enum_val(0)); // Sun
    sun_node.params.insert("pos_x".to_string(), float(5.0));
    sun_node.params.insert("pos_y".to_string(), float(2.0));
    sun_node.params.insert("pos_z".to_string(), float(3.0));
    sun_node.params.insert("aim_x".to_string(), float(0.0));
    sun_node.params.insert("aim_y".to_string(), float(0.0));
    sun_node.params.insert("aim_z".to_string(), float(0.0));
    // 3.5, not the primitive's 1.5 default: `node.pbr_material` divides diffuse
    // by π (energy conservation), so a unit-intensity sun lands a fully-facing
    // matte surface at ~0.32 — a dark subject then reads near-black. ~3.5
    // offsets the /π so an imported model is legible under the default rig
    // without the user having to touch the light. Aesthetic default; the light
    // node is a normal graph node the user can dial down.
    sun_node.params.insert("intensity".to_string(), float(3.5));
    // Hard shadow softness: crisp, defined shadows suit the dramatic
    // single-key "model on black" look. The Shadow Type card lets the user
    // soften to Soft/Very Soft/Contact. `light_size` is inert for Hard (no
    // penumbra), kept at 1.0 so a switch to a softer tier gives a sensible
    // penumbra width.
    sun_node.params.insert("cast_shadows".to_string(), float(1.0));
    sun_node.params.insert("shadow_softness".to_string(), enum_val(0)); // Hard
    sun_node.params.insert("light_size".to_string(), float(1.0));
    // Shadow quality: the Sun's `range` is the shadow's orthographic
    // half-extent, and the shadow map's texels spread across it. The default
    // 30 wraps a huge area, so on a recentered hero mesh (spanning ±radius
    // about the origin) almost none of the map's texels land on the subject —
    // that's the blocky, texel-stepping look as the sun moves. Wrap the extent
    // tightly to the model (radius × 1.5 for margin) and quadruple the map to
    // 4096², so the shadow texels are fine enough that edges read crisp and
    // per-frame motion stops stepping. Both are normal light-node params the
    // user can still dial.
    sun_node.params.insert("range".to_string(), float((radius * 1.5).max(0.01)));
    sun_node.params.insert("shadow_resolution".to_string(), float(4096.0));
    let sun_node_params = sun_node.params.clone();
    nodes.push(sun_node);
    stamp_scene_node_exposures_into(
        &mut card_params,
        &mut card_bindings,
        sun_id,
        &NodeId::new("sun"),
        "node.light",
        "Sun",
        &metadata_for_node_type("node.light"),
        &sun_node_params,
    );

    let render_id = fresh_id();
    let mut render_node = plain_node(render_id, "render", "node.render_scene", "render");
    render_node.params.insert("objects".to_string(), int(n as i32));
    render_node.params.insert("lights".to_string(), int(1));
    // RAYTRACING_DESIGN.md D14/§5.2: stamp the root's curated RT subset
    // (RENDER_SCENE_STAMPED_PARAMS) like every other vocab node above —
    // without it a fresh import has no "Rendering" rows until a save/reload
    // runs the load-time migration.
    stamp_scene_node_exposures_into(
        &mut card_params,
        &mut card_bindings,
        render_id,
        &NodeId::new("render"),
        "node.render_scene",
        "Rendering",
        &metadata_for_node_type("node.render_scene"),
        &render_node.params.clone(),
    );

    let mut string_bindings = Vec::new();
    let mut textures_wired = 0usize;
    let mut any_animated = false;
    // D9 doctrine — see `ImportReport::report_lines`.
    let mut report_lines: Vec<String> = Vec::new();

    // Group names, deduped so two identically-named glTF materials don't collide
    // in the flattened handle map (the flattener prefixes every inner handle with
    // the group name).
    let mut used_group_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    // GLTF_ANIMATION_DESIGN.md A2: clip `[0]`'s per-node TRS tracks, keyed
    // by node index — the same lookup `gltf_load::gltf_import_summary`
    // builds internally for A1's rigid-object resolution, rebuilt here
    // because `node.gltf_skeleton_pose`'s Tables need it per-JOINT (every
    // joint in a skin, not just the one mesh-owning node A1 resolves
    // against). A4: ALL clips, not just clip 0 (D4 multi-clip selection).
    let node_anims_by_clip: Vec<std::collections::BTreeMap<usize, gltf_load::GltfNodeAnimation>> =
        summary.animations.iter().map(|a| a.nodes.iter().map(|n| (n.node_index, n.clone())).collect()).collect();

    let mut import_ctx = ImportCtx {
        render_id,
        path_str: &path_str,
        center,
        bbox_radius: radius,
        node_anims_by_clip: &node_anims_by_clip,
        used_group_names: &mut used_group_names,
        fresh_id: &mut fresh_id,
    };
    for (k, m) in materials.iter().enumerate() {
        let mut out = build_object_group(&mut import_ctx, k, k, m, "anim");
        nodes.push(out.group_node);
        wires.append(&mut out.wires_to_render);
        card_params.append(&mut out.card_params);
        card_bindings.append(&mut out.card_bindings);
        string_bindings.append(&mut out.string_bindings);
        report_lines.append(&mut out.report_lines);
        textures_wired += out.textures_wired;
        any_animated |= out.animated;
    }

    // One "Animation" section per glb: clips are
    // file-level, so Rate/Clip/Loop/Retrigger are a single linked control
    // fanning out to every animation clock the loop above bound. Prepended
    // so the section leads the card, ahead of the per-object sections.
    if any_animated {
        let mut anim_params = Vec::new();
        animation_card_params(
            &mut anim_params,
            "anim",
            "Animation",
            summary.animations.len().max(1) as u32,
            clip_labels(&summary.animations),
        );
        anim_params.append(&mut card_params);
        card_params = anim_params;
    }

    nodes.push(render_node);

    // No atmosphere node: the
    // BUG-149 scene-scaled fog and shaft knobs never produced the look he
    // wanted on imports — the cinematic void-haze treatment is a pending
    // design of its own (`project_void_haze_design_pending`), not two
    // faders on this card. render_scene's `atmosphere` input is lazy, so
    // leaving it unwired is byte-identical to the old wired-at-defaults
    // node with both sliders at 0.

    // SSAO contact-occlusion arm, packaged as the same "ao" node group
    // CinematicScene ships (CINEMATIC_POST D9): ssao_gtao → bilateral_blur
    // (H then V) denoise → Multiply onto render_scene's color. Grouped so
    // the graph reads as one labeled box, same as every object group above;
    // flattens to the identical atom wiring at load
    // (`manifold_core::flatten::flatten_groups`, docs/NODE_GROUPS_DESIGN.md).
    //
    // SSAO radius is a WORLD-space distance and the importer never rescales
    // the model, so a hero mesh can be fractions of a unit or hundreds across.
    // Scale the default to the model's own bounding radius (kept inside the
    // atom's declared 0.01..5.0 envelope) so contact shadows read at any size.
    let ssao_radius_default = (radius * 0.5).clamp(0.01, 5.0);

    let mut ao_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut ao_wires: Vec<EffectGraphWire> = Vec::new();
    let ao_in_id = fresh_id();
    ao_nodes.push(plain_node(ao_in_id, "ao_in", GROUP_INPUT_TYPE_ID, "input"));
    let ssao_id = fresh_id();
    let mut ssao_node = plain_node(ssao_id, "ssao", "node.ssao_gtao", "ssao");
    ssao_node.params.insert("radius".to_string(), float(ssao_radius_default));
    ssao_node.params.insert("intensity".to_string(), float(1.0));
    ao_nodes.push(ssao_node);
    let ssao_mix_id = fresh_id();
    let mut ssao_mix_node = plain_node(ssao_mix_id, "ssao_mix", "node.mix", "ssao_mix");
    ssao_mix_node.params.insert("amount".to_string(), float(1.0));
    ssao_mix_node.params.insert("mode".to_string(), enum_val(4)); // Multiply
    ao_nodes.push(ssao_mix_node);
    let bilat_h_id = fresh_id();
    let mut bilat_h_node = plain_node(bilat_h_id, "bilat_h", "node.bilateral_blur", "bilat_h");
    bilat_h_node.params.insert("axis".to_string(), enum_val(0));
    bilat_h_node.params.insert("depth_sigma".to_string(), float(0.1));
    ao_nodes.push(bilat_h_node);
    let bilat_v_id = fresh_id();
    let mut bilat_v_node = plain_node(bilat_v_id, "bilat_v", "node.bilateral_blur", "bilat_v");
    bilat_v_node.params.insert("axis".to_string(), enum_val(1));
    bilat_v_node.params.insert("depth_sigma".to_string(), float(0.1));
    ao_nodes.push(bilat_v_node);
    let ao_out_id = fresh_id();
    ao_nodes.push(plain_node(ao_out_id, "ao_out", GROUP_OUTPUT_TYPE_ID, "output"));
    ao_wires.push(wire(ao_in_id, "depth", ssao_id, "depth"));
    ao_wires.push(wire(ao_in_id, "camera", ssao_id, "camera"));
    ao_wires.push(wire(ao_in_id, "color", ssao_mix_id, "a"));
    ao_wires.push(wire(ssao_id, "out", bilat_h_id, "in"));
    ao_wires.push(wire(ao_in_id, "depth", bilat_h_id, "depth"));
    ao_wires.push(wire(ao_in_id, "camera", bilat_h_id, "camera"));
    ao_wires.push(wire(bilat_h_id, "out", bilat_v_id, "in"));
    ao_wires.push(wire(ao_in_id, "depth", bilat_v_id, "depth"));
    ao_wires.push(wire(ao_in_id, "camera", bilat_v_id, "camera"));
    ao_wires.push(wire(bilat_v_id, "out", ssao_mix_id, "b"));
    ao_wires.push(wire(ssao_mix_id, "out", ao_out_id, "out"));

    let ao_group_id = fresh_id();
    let mut ao_group_node = plain_node(ao_group_id, "ao", GROUP_TYPE_ID, "ao");
    ao_group_node.title = Some("Ambient Occlusion".to_string());
    ao_group_node.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: vec![
                InterfacePortDef { name: "depth".to_string(), port_type: "Texture2D".to_string() },
                InterfacePortDef { name: "camera".to_string(), port_type: "Camera".to_string() },
                InterfacePortDef { name: "color".to_string(), port_type: "Texture2D".to_string() },
            ],
            outputs: vec![InterfacePortDef { name: "out".to_string(), port_type: "Texture2D".to_string() }],
            params: Vec::new(),
        },
        nodes: ao_nodes,
        wires: ao_wires,
        tint: None,
    }));
    nodes.push(ao_group_node);

    // Depth of Field group removed: the coc_dilate/
    // bokeh_gather chain read as buggy in practice and made imported scenes
    // hard to look at. `render_scene → ao → final` now, no dof stage; the
    // "lens" node stays — it still feeds render_scene's/ao's `camera` input
    // and the FOV card knob.

    // No `node.motion_blur` tail: it's live-tracked as BUG-136 (MED-HIGH,
    // `docs/BUG_BACKLOG.md`) — orbiting the camera in the running app
    // produces no visible blur despite the wiring, shader math, and
    // velocity buffer all having been runtime-confirmed correct through
    // exhaustive headless probing. The unresolved suspects live in the
    // live app's UI-thread param propagation / render-loop scheduling,
    // outside what this assembler controls. It's also disproportionately
    // expensive for a no-op: `graph_tool fusion` on the assembled import
    // graph shows the whole GTAO/DoF chain (and this node too, had it
    // stayed) failing to fuse into anything but one 2-node region — every
    // atom pays its own dispatch (BUG-141, also open) — so a broken,
    // always-on 32px gather was pure cost with no visual payoff. Retry
    // once BUG-136 lands a root cause.
    let final_id = fresh_id();
    nodes.push(plain_node(final_id, "final", FINAL_OUTPUT_TYPE_ID, "final"));

    // The lens sits between the raw orbit camera and every downstream
    // consumer (render_scene, the ao group) — the node whose fov_y the
    // Camera FOV card slider reads.
    wires.push(wire(camera_id, "out", lens_id, "camera"));
    wires.push(wire(lens_id, "out", render_id, "camera"));
    // D6 env_mode switch: envmap(bake) -> in_0 (Softbox), hdri -> gain ->
    // in_1 (HDRI, with the exposure stage — see the hdri_gain node above),
    // the switch's `out` feeds render_scene same as the direct wire used to.
    wires.push(wire(envmap_id, "envmap", env_select_id, "in_0"));
    wires.push(wire(hdri_id, "out", hdri_gain_id, "in"));
    wires.push(wire(hdri_gain_id, "out", env_select_id, "in_1"));
    wires.push(wire(env_select_id, "out", render_id, "envmap"));
    wires.push(wire(sun_id, "out", render_id, "light_0"));

    // render_scene → ao (contact AO) → final.
    wires.push(wire(render_id, "depth", ao_group_id, "depth"));
    wires.push(wire(lens_id, "out", ao_group_id, "camera"));
    wires.push(wire(render_id, "color", ao_group_id, "color"));
    wires.push(wire(ao_group_id, "out", final_id, "in"));

    // P1: scene-vocabulary atoms (camera, lens, sun, envmap) are already
    // exposed above from their primitive ParamDefs. This block keeps the
    // curated cross-node fan-outs and shared controls that are not covered by
    // a single node's manifest.
    //
    // D7 sun coherence: the sun pos sliders also drive envmap.sun_x/y/z so one
    // control moves both the light and the reflection direction. Use the same
    // default as the auto-generated sun binding so the slider rests
    // consistently on both targets.
    for (param, axis) in [("pos_x", "sun_x"), ("pos_y", "sun_y"), ("pos_z", "sun_z")] {
        let id = format!("{sun_id}_{param}");
        let default = card_bindings
            .iter()
            .find(|b| b.id == id)
            .map(|b| b.default_value)
            .unwrap_or(0.0);
        card_bindings.push(BindingDef {
            id,
            label: String::new(),
            default_value: default,
            target: BindingTarget::Node {
                node_id: NodeId::new("envmap"),
                param: axis.to_string(),
            },
            convert: manifold_core::effects::ParamConvert::Float,
            user_added: false,
            scale: 1.0,
            offset: 0.0,
        });
    }
    // Environment intensity also drives the HDRI exposure gain (G-P6).
    let env_intensity_id = format!("{envmap_id}_intensity");
    let env_intensity_default = card_bindings
        .iter()
        .find(|b| b.id == env_intensity_id)
        .map(|b| b.default_value)
        .unwrap_or(1.0);
    card_bindings.push(BindingDef {
        id: env_intensity_id,
        label: String::new(),
        default_value: env_intensity_default,
        target: BindingTarget::Node {
            node_id: NodeId::new("hdri_gain"),
            param: "gain".to_string(),
        },
        convert: manifold_core::effects::ParamConvert::Float,
        user_added: false,
        scale: 1.0,
        offset: 0.0,
    });

    // HDRI mode switch: `node.switch_texture` is not scene vocabulary, so it is
    // not auto-exposed; keep the curated enum.
    let mut env_mode_param = card_param("env_mode", "Environment Mode", 0.0, 1.0, 0.0, false, "Environment");
    env_mode_param.whole_numbers = true;
    env_mode_param.value_labels = vec!["Softbox".to_string(), "HDRI".to_string()];
    card_params.push(env_mode_param);
    card_bindings.push(card_binding(
        "env_mode", "Environment Mode", 0.0, "env_select", "selector", 1.0,
    ));
    // The HDRI file itself is a SEPARATE Browse field/string param from
    // "model_file" (the imported .glb's own path) — a different file, a
    // different picker, never defaulted to the glb path. Empty until the
    // performer picks one; `node.hdri_source` reads that as "nothing decoded
    // yet" and clears `out` to black (step 6 of its `run()`), which env_mode=0
    // (Softbox, the default) never reaches anyway.
    string_bindings.push(StringBindingDef {
        id: HDRI_FILE_PARAM_ID.to_string(),
        label: "HDRI File".to_string(),
        default_value: String::new(),
        target: BindingTarget::Node {
            node_id: NodeId::new("hdri"),
            param: "path".to_string(),
        },
    });

    // The shared Ambient fill knob (its per-material bindings were fanned out
    // in the object loop above). 0.0 = no flat fill (lights-only); raise it to
    // lift the shadow side of every material at once.
    card_params.push(card_param("scene_ambient", "Ambient", 0.0, 1.0, 0.0, false, "Environment"));

    // No SSAO card sliders (Peter, 2026-07-15: "the defaults look good") —
    // the `ao` node group stays wired at its defaults
    // (`ssao_radius_default`/1.0 intensity, set on the ssao node above);
    // it's just no longer exposed on the outer card.

    // No Atmosphere section: fog + god rays removed with the atmosphere
    // node — see the removal comment in
    // `build_import_graph`.

    // Category "Geometry" matches the existing 3D-geometry generator
    // convention (Tesseract / DigitalPlants / NestedCubes / Duocylinder /
    // Wireframe) — closer to this preset's actual content than any entry
    // in `preset_type_registry::ALL_CATEGORIES`, which is an EFFECT-picker
    // bucket list (generators carry no category validation; see that
    // module's doc comment — "Text & Media" on `MriVolume`, a generator,
    // already isn't in that list either).
    let metadata = PresetMetadata {
        id: PresetTypeId::from_string(sanitized),
        display_name: stem.clone(),
        category: "Geometry".to_string(),
        osc_prefix,
        legacy_discriminant: None,
        available: true,
        is_line_based: false,
        params: card_params,
        bindings: card_bindings,
        skip_mode: SkipModeDef::default(),
        param_aliases: Vec::new(),
        value_aliases: Vec::new(),
        string_params: vec![
            StringParamSpecDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                name: "Model File".to_string(),
                default_value: path_str,
                is_file_picker: true,
                use_dropdown: false,
            },
            StringParamSpecDef {
                id: HDRI_FILE_PARAM_ID.to_string(),
                name: "HDRI File".to_string(),
                default_value: String::new(),
                is_file_picker: true,
                use_dropdown: false,
            },
        ],
        string_bindings,
    };

    let def = EffectGraphDef {
        version: manifold_core::effect_graph_def::EFFECT_GRAPH_VERSION,
        name: None,
        description: None,
        preset_metadata: None,
        nodes,
        wires,
    }
    .with_name(stem)
    .with_description(format!("Imported from {}", path.display()))
    .with_preset_metadata(metadata);

    // GLTF_ANIMATION_DESIGN.md A1: fold the parse-time animation findings
    // (non-LINEAR channels, morph-weight channels, multi-node-per-material
    // conflicts) into the same never-silent report doctrine as every
    // other D9 line above.
    report_lines.extend(summary.animation_report_lines.iter().cloned());
    // BUG-213: same fold for unimplemented OPTIONAL extensionsUsed entries.
    report_lines.extend(summary.extension_report_lines.iter().cloned());

    let report = ImportReport {
        material_count: summary.materials.len(),
        object_count: n,
        textures_wired,
        default_material_vertex_count: summary.default_material_vertex_count,
        camera_synthesized: true,
        report_lines,
    };

    Ok((def, report))
}
