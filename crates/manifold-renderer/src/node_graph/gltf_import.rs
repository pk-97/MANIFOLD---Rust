//! glTF import ASSEMBLER (P1c stage 2) — a pure function that turns a
//! parsed `.glb`'s [`gltf_load::GltfImportSummary`] (stage 1: the CPU-only
//! parse) into a generator [`EffectGraphDef`] that renders the model
//! faithfully: one `node.render_scene` object PER DISTINCT MATERIAL, each
//! fed its material-filtered geometry (`node.gltf_mesh_source`), that
//! material's base-color texture (`node.gltf_texture_source`, when present),
//! a `node.pbr_material` atom carrying the glTF's PBR factors, and a
//! `node.transform_3d` atom (seeded to recenter the object at the origin) —
//! plus a shared synthesized framing camera (`node.orbit_camera`), a sun
//! light (`node.light`), and an IBL envmap (`node.bake_environment`).
//!
//! Each object's producers are wrapped in one named, tinted node **group**
//! so a multi-mesh import reads as a few labelled boxes in the graph editor
//! rather than a wall of loose nodes; the group flattens away at load to the
//! exact same flat graph, so nothing the runtime sees changes (see
//! [`build_import_graph`] and `docs/GROUPING_GRAPHS.md`).
//!
//! No GPU, no file I/O beyond the one [`gltf_load::gltf_import_summary`]
//! parse this module drives — everything here is graph-shape assembly.
//! The glb path itself never becomes a node `param` (there is no `String`
//! variant in [`SerializedParamValue`] — see its doc comment); it flows
//! through `presetMetadata.stringParams` + `stringBindings`, the same
//! outer-card text-config convention `node.image_folder`-based presets use
//! (see `assets/generator-presets/MriVolume.json`'s `axial_folder`).
//!
//! Production caller: `manifold-app`'s `.glb`/`.gltf` file-drop handler
//! (`Application::import_model_file`) calls [`assemble_import_graph`], then
//! installs the result on a new generator layer via
//! `manifold_editing::commands::layer::ImportModelLayerCommand`.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use manifold_core::NodeId;
use manifold_core::PresetTypeId;
use manifold_core::effect_graph_def::{
    BindingDef, BindingTarget, EffectGraphDef, EffectGraphNode, EffectGraphWire, GROUP_INPUT_TYPE_ID,
    GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, GroupDef, GroupInterface, InterfacePortDef, ParamSpecDef,
    PresetMetadata, SerializedParamValue, SkipModeDef, StringBindingDef, StringParamSpecDef,
};

use super::boundary_nodes::{FINAL_OUTPUT_TYPE_ID, GENERATOR_INPUT_TYPE_ID};
use super::gltf_load;
use super::gltf_load::GltfImportSummary;

use crate::node_graph::primitives::render_scene::OBJECT_SLIDER_MAX;

/// Stable identity for the one outer-card text config every imported
/// preset carries: the source `.glb`/`.gltf` path.
const MODEL_FILE_PARAM_ID: &str = "model_file";

/// What the assembler did, for the caller (importer UI, tests) to report
/// or warn on. Not part of the graph itself.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// Distinct materials with geometry, as parsed (before the
    /// [`OBJECT_SLIDER_MAX`] truncation threshold).
    pub material_count: usize,
    /// Objects actually wired into `node.render_scene` — `min(material_count, OBJECT_SLIDER_MAX)`.
    pub object_count: usize,
    /// How many objects got a `node.gltf_texture_source` → `base_color_map_N` wire.
    pub textures_wired: usize,
    /// Materials dropped because the glb has more than `OBJECT_SLIDER_MAX`
    /// (the smallest by vertex count, so the most visually significant
    /// objects survive).
    pub dropped_over_cap: usize,
    /// Triangle-list vertices belonging to glTF's unassigned default
    /// material — v1 does not import these (mirrors
    /// [`gltf_load::GltfImportSummary::default_material_vertex_count`]).
    pub default_material_vertex_count: u32,
    /// Always `true` today — the assembler always synthesizes a framing
    /// camera (the glb's own embedded cameras, if any, are not yet
    /// consumed). Kept as a field so a future embedded-camera path has
    /// somewhere to report `false`.
    pub camera_synthesized: bool,
}

/// Build an [`EffectGraphNode`] with the given identity and every other
/// field at its "ordinary node" default. `EffectGraphNode` doesn't derive
/// `Default` (several fields are meaningful `Option`s used by grouping /
/// the graph editor), so this centralises the shape once rather than
/// repeating all eleven fields at every call site.
fn plain_node(id: u32, node_id: &str, type_id: &str, handle: &str) -> EffectGraphNode {
    EffectGraphNode {
        id,
        node_id: NodeId::new(node_id),
        type_id: type_id.to_string(),
        handle: Some(handle.to_string()),
        params: BTreeMap::new(),
        exposed_params: BTreeSet::new(),
        editor_pos: None,
        wgsl_source: None,
        title: None,
        output_formats: BTreeMap::new(),
        output_canvas_scales: BTreeMap::new(),
        group: None,
    }
}

fn wire(from_node: u32, from_port: &str, to_node: u32, to_port: &str) -> EffectGraphWire {
    EffectGraphWire {
        from_node,
        from_port: from_port.to_string(),
        to_node,
        to_port: to_port.to_string(),
    }
}

fn float(v: f32) -> SerializedParamValue {
    SerializedParamValue::Float { value: v }
}
fn int(v: i32) -> SerializedParamValue {
    SerializedParamValue::Int { value: v }
}
fn enum_val(v: u32) -> SerializedParamValue {
    SerializedParamValue::Enum { value: v }
}

/// Degrees→radians factor for the camera card sliders. The camera node's
/// `orbit`/`tilt`/`fov_y` params are radians (matching `node.orbit_camera`),
/// but a performer wants degrees on the card, so each angle slider carries a
/// [`BindingDef::scale`] of this value — the `param_binding` write boundary
/// applies `value * scale` (no `deg_to_rad` helper node needed, unlike the
/// hand-authored MetallicGlass preset).
const DEG2RAD: f32 = std::f32::consts::PI / 180.0;

/// One outer-card slider definition. Curated performance surface for an
/// imported model (camera framing, light, material) — see
/// [`assemble_import_graph`]'s metadata block. `default_value` must match
/// the wired node param's value (through `scale`) so the card reproduces the
/// assembler's look on first frame with no drift. `section` bundles this
/// knob under a collapsible card header (D5/D9,
/// SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2): per-object knobs get the
/// object's group name, shared knobs get `"Camera"`/`"Sun"`/`"Environment"`.
fn card_param(
    id: &str,
    name: &str,
    min: f32,
    max: f32,
    default: f32,
    is_angle: bool,
    section: &str,
) -> ParamSpecDef {
    ParamSpecDef {
        id: id.to_string(),
        name: name.to_string(),
        min,
        max,
        default_value: default,
        whole_numbers: false,
        is_toggle: false,
        is_trigger: false,
        value_labels: Vec::new(),
        format_string: None,
        osc_suffix: String::new(),
        curve: manifold_core::macro_bank::MacroCurve::default(),
        invert: false,
        is_angle,
        is_trigger_gate: false,
        wraps: false,
        section: Some(section.to_string()),
    }
}

/// Route one card slider (`id`) to one inner node param. `scale` folds a
/// unit conversion (e.g. [`DEG2RAD`]) into the write boundary; pass `1.0`
/// for a pass-through. `default_value` mirrors the matching [`card_param`]'s
/// so the slider's fallback (when a project carries no `param_values` slot)
/// still reproduces the authored look.
fn card_binding(
    id: &str,
    name: &str,
    default: f32,
    node_id: &str,
    param: &str,
    scale: f32,
) -> BindingDef {
    BindingDef {
        id: id.to_string(),
        label: name.to_string(),
        default_value: default,
        target: BindingTarget::Node {
            node_id: NodeId::new(node_id),
            param: param.to_string(),
        },
        convert: manifold_core::effects::ParamConvert::Float,
        user_added: false,
        scale,
        offset: 0.0,
    }
}

/// Replace every run of non-alphanumeric characters with a single `_` and
/// trim leading/trailing underscores, so a filename stem like
/// `"cc0__oomurasaki_azalea_r._x_pulchrum"` becomes a clean identifier
/// safe for a [`PresetTypeId`] / OSC path segment. Falls back to
/// `"ImportedModel"` for a stem that sanitizes to nothing, and prefixes a
/// leading digit (OSC/identifier convention) with `Model_`.
fn sanitize_identifier(stem: &str) -> String {
    let mut out = String::with_capacity(stem.len());
    let mut prev_underscore = false;
    for c in stem.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c);
            prev_underscore = false;
        } else if !prev_underscore {
            out.push('_');
            prev_underscore = true;
        }
    }
    let trimmed = out.trim_matches('_');
    if trimmed.is_empty() {
        "ImportedModel".to_string()
    } else if trimmed.chars().next().is_some_and(|c| c.is_ascii_digit()) {
        format!("Model_{trimmed}")
    } else {
        trimmed.to_string()
    }
}

/// Display / namespace name for one object's group: the glTF material name when
/// present (with the reserved `/` swapped for a space — the flattener uses `/`
/// as the handle namespace separator), else `"Object N"`. Deduped against
/// siblings — two same-named materials would otherwise collide in the flattened
/// handle map — by appending an index until unique.
fn unique_group_name(
    material_name: Option<&str>,
    index: usize,
    used: &mut std::collections::HashSet<String>,
) -> String {
    let base = material_name
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| s.replace('/', " "))
        .unwrap_or_else(|| format!("Object {}", index + 1));
    let mut name = base.clone();
    let mut n = index + 1;
    while used.contains(&name) {
        name = format!("{base} {n}");
        n += 1;
    }
    used.insert(name.clone());
    name
}

/// A distinct RGBA header tint for object `index`, spread around the hue wheel by
/// the golden ratio (the same scheme `Layer::generate_layer_color` uses for
/// timeline layers) at high saturation — so a multi-mesh import reads as a few
/// colour-coded boxes, never a wall of same-coloured groups.
fn group_tint(index: usize) -> [f32; 4] {
    let hue = (index as f32 * 0.618_034) % 1.0;
    let c = manifold_core::color::Color::hsv_to_rgb(hue, 0.7, 0.85);
    [c.r, c.g, c.b, 1.0]
}

/// Parse `path` and assemble a generator [`EffectGraphDef`] that renders it
/// faithfully: one `node.render_scene` object per distinct material
/// (capped at [`OBJECT_SLIDER_MAX`], largest-by-vertex-count first),
/// each fed its material-filtered geometry + base-color texture (if any) +
/// a PBR material, framed by a synthesized orbit camera sized to the glb's
/// bounding box, lit by one sun light, under a baked IBL envmap (required —
/// `node.pbr_material` is degenerate without one). Pure function: one CPU
/// parse via [`gltf_load::gltf_import_summary`], no GPU, no other I/O.
///
/// Errors when the glb has no materials with geometry (nothing to import) —
/// propagated from [`gltf_load::gltf_import_summary`] or raised here.
pub fn assemble_import_graph(path: &Path) -> Result<(EffectGraphDef, ImportReport), String> {
    let summary = gltf_load::gltf_import_summary(path)?;
    build_import_graph(&summary, path)
}

/// Assemble the generator graph from an already-parsed [`GltfImportSummary`].
/// Split from [`assemble_import_graph`] (which owns the single file parse) so the
/// graph shape — including the per-object node **grouping** — is testable against
/// a synthetic summary with no `.glb` fixture on disk.
///
/// Each distinct material becomes one node **group** (`GROUP_TYPE_ID`) named for
/// the material: its `node.gltf_mesh_source` + `node.pbr_material` +
/// `node.transform_3d` (+ optional `node.gltf_texture_source`) live inside,
/// exposed through a `system.group_output` as `vertices` / `material` /
/// `transform` / `baseColor`, and the group's outputs wire to the
/// shared `node.render_scene`. Grouping is a pure presentation layer: it flattens
/// away at load (`manifold_core::flatten::flatten_groups`, run inside
/// `instantiate_def`) to the exact same flat graph, and every inner node keeps its
/// stable `node_id`, so the card/string bindings that target `mesh_k`/`mat_k`/
/// `tex_k`/`transform_k` by id resolve unchanged (see `docs/GROUPING_GRAPHS.md` §2).
fn build_import_graph(
    summary: &GltfImportSummary,
    path: &Path,
) -> Result<(EffectGraphDef, ImportReport), String> {
    if summary.materials.is_empty() {
        return Err(format!(
            "{}: no materials with geometry — nothing to import",
            path.display()
        ));
    }

    // Largest-by-vertex-count first, so a >OBJECT_SLIDER_MAX-material glb
    // keeps its most visually significant objects when capped, not an
    // arbitrary prefix.
    let object_cap = OBJECT_SLIDER_MAX as usize;
    let mut materials = summary.materials.clone();
    materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
    let dropped_over_cap = materials.len().saturating_sub(object_cap);
    materials.truncate(object_cap);
    let n = materials.len();
    if dropped_over_cap > 0 {
        log::warn!(
            "gltf_import::assemble_import_graph({}): {} materials with geometry, \
             node.render_scene caps at {object_cap} objects — dropping the \
             {dropped_over_cap} smallest by vertex count",
            path.display(),
            summary.materials.len(),
        );
    }
    if summary.default_material_vertex_count > 0 {
        log::warn!(
            "gltf_import::assemble_import_graph({}): {} vertices belong to glTF's unassigned \
             default material — v1 does not import these",
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
    let distance = 2.2 * radius;

    let path_str = path.to_string_lossy().into_owned();
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "ImportedModel".to_string());
    let sanitized = sanitize_identifier(&stem);
    let osc_prefix = sanitized.to_lowercase();

    let mut nodes = Vec::new();
    let mut wires = Vec::new();
    let mut next_id = 0u32;
    let mut fresh_id = move || {
        let v = next_id;
        next_id += 1;
        v
    };

    let input_id = fresh_id();
    nodes.push(plain_node(input_id, "input", GENERATOR_INPUT_TYPE_ID, "input"));

    // The IBL envmap is still baked and wired (node.pbr_material needs an
    // envmap bound), but at intensity 0 the map is fully black — so PBR objects
    // receive no image-based lighting and are lit purely by the scene's lights,
    // the hard dramatic "model on black" look. The Environment card below turns
    // it back up for a softer, reflective studio look.
    let envmap_id = fresh_id();
    let mut envmap_node = plain_node(envmap_id, "envmap", "node.bake_environment", "envmap");
    envmap_node.params.insert("intensity".to_string(), float(0.0));
    nodes.push(envmap_node);

    let camera_id = fresh_id();
    let mut cam_node = plain_node(camera_id, "camera", "node.orbit_camera", "camera");
    cam_node.params.insert("orbit".to_string(), float(0.7));
    cam_node.params.insert("tilt".to_string(), float(0.3));
    cam_node.params.insert("distance".to_string(), float(distance));
    cam_node.params.insert("fov_y".to_string(), float(0.9));
    cam_node.params.insert("look_y".to_string(), float(0.0));
    nodes.push(cam_node);

    // Physical lens (CINEMATIC_POST D6): the node the DoF group's
    // circle-of-confusion reads. No motion_blur consumer wired (see the
    // motion_blur removal note below), so `shutter_angle` is left at the
    // primitive's own neutral default (0) rather than set here. f_stop
    // starts at the primitive's own neutral top-of-range (32 — matches
    // CinematicScene's declared max), so an import looks unchanged until
    // the Depth of Field card is dialed in. focus_distance seeds to the
    // synthesized camera's own framing distance so the subject is the
    // first thing that stays sharp once f_stop drops.
    let lens_id = fresh_id();
    let mut lens_node = plain_node(lens_id, "lens", "node.camera_lens", "lens");
    lens_node.params.insert("focus_distance".to_string(), float(distance));
    lens_node.params.insert("f_stop".to_string(), float(32.0));
    nodes.push(lens_node);

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
    nodes.push(sun_node);

    let render_id = fresh_id();
    let mut render_node = plain_node(render_id, "render", "node.render_scene", "render");
    render_node.params.insert("objects".to_string(), int(n as i32));
    render_node.params.insert("lights".to_string(), int(1));

    let mut string_bindings = Vec::new();
    // Curated outer-card performance surface (D9 / P0). Camera + light +
    // envmap are added once after the loop; per-object material knobs are
    // pushed here so they interleave with the `mat_k` nodes they target.
    let mut card_params: Vec<ParamSpecDef> = Vec::new();
    let mut card_bindings: Vec<BindingDef> = Vec::new();
    let mut textures_wired = 0usize;

    // Group names, deduped so two identically-named glTF materials don't collide
    // in the flattened handle map (the flattener prefixes every inner handle with
    // the group name).
    let mut used_group_names: std::collections::HashSet<String> = std::collections::HashSet::new();

    for (k, m) in materials.iter().enumerate() {
        let mesh_node_id = format!("mesh_{k}");
        let mat_node_id = format!("mat_{k}");
        // Computed up front (not just before the group box below) so the
        // per-object card knobs pushed further down can stamp it as their
        // `section` (D5/D9) — the section now carries the per-object
        // identity the old " 2"-style label suffix used to.
        let group_name = unique_group_name(m.name.as_deref(), k, &mut used_group_names);

        // This object's producer nodes live INSIDE its group; only the group box
        // and the shared render / camera / lights / boundaries sit at the top
        // level (the spine). Every inner node keeps its stable `node_id`, so the
        // card + string bindings below still resolve after the load-time flatten.
        let mut group_nodes: Vec<EffectGraphNode> = Vec::new();
        let mut group_wires: Vec<EffectGraphWire> = Vec::new();

        let mesh_id = fresh_id();
        let mut mesh_node =
            plain_node(mesh_id, &mesh_node_id, "node.gltf_mesh_source", &mesh_node_id);
        mesh_node
            .params
            .insert("material_index".to_string(), int(m.material_index as i32));
        mesh_node
            .params
            .insert("max_capacity".to_string(), int(m.vertex_count.max(1) as i32));
        group_nodes.push(mesh_node);

        let mat_id = fresh_id();
        let mut mat_node = plain_node(mat_id, &mat_node_id, "node.pbr_material", &mat_node_id);
        // Author the glTF material's own name as the node's display title
        // when present, so the graph editor reads as "Leaf" / "Bark" rather
        // than the anonymous "mat_0" / "mat_1" handle.
        mat_node.title = m.name.clone();
        mat_node
            .params
            .insert("color_r".to_string(), float(m.base_color_factor[0]));
        mat_node
            .params
            .insert("color_g".to_string(), float(m.base_color_factor[1]));
        mat_node
            .params
            .insert("color_b".to_string(), float(m.base_color_factor[2]));
        mat_node
            .params
            .insert("color_a".to_string(), float(m.base_color_factor[3]));
        mat_node.params.insert("metallic".to_string(), float(m.metallic));
        mat_node
            .params
            .insert("roughness".to_string(), float(m.roughness.max(0.01)));
        // 0.0 ambient: no flat fill floor, so the shadow side of a matte model
        // goes to true black under the default single-key rig — the hard,
        // dramatic "lit only by scene lights" look. The shared Ambient card
        // (below) raises it across every material to restore fill.
        mat_node.params.insert("ambient".to_string(), float(0.0));
        mat_node
            .params
            .insert("emission_r".to_string(), float(m.emissive[0]));
        mat_node
            .params
            .insert("emission_g".to_string(), float(m.emissive[1]));
        mat_node
            .params
            .insert("emission_b".to_string(), float(m.emissive[2]));
        let emissive_lit = m.emissive.iter().any(|&c| c > 0.0);
        mat_node.params.insert(
            "emission_intensity".to_string(),
            float(if emissive_lit { 1.0 } else { 0.0 }),
        );
        mat_node
            .params
            .insert("alpha_mode".to_string(), enum_val(if m.alpha_mask { 1 } else { 0 }));
        mat_node
            .params
            .insert("alpha_cutoff".to_string(), float(m.alpha_cutoff));
        group_nodes.push(mat_node);

        // Per-object material knobs on the card: metallic + roughness, the
        // two live PBR controls. No more " 2"-style numeric suffix on the
        // label — the `section` (the object's own group name) now carries
        // that identity, so every object reads "Metallic" / "Roughness"
        // under its own collapsible header (D5/D9). Defaults mirror the
        // node params set just above, so the card reproduces the glTF's own
        // material on first frame. These still target the material node by its
        // stable `node_id`, which grouping preserves.
        let metal_default = m.metallic;
        let rough_default = m.roughness.max(0.01);
        let metal_id = format!("metal_{k}");
        let rough_id = format!("rough_{k}");
        card_params.push(card_param(&metal_id, "Metallic", 0.0, 1.0, metal_default, false, &group_name));
        card_bindings.push(card_binding(
            &metal_id, "Metallic", metal_default, &mat_node_id, "metallic", 1.0,
        ));
        card_params.push(card_param(&rough_id, "Roughness", 0.01, 1.0, rough_default, false, &group_name));
        card_bindings.push(card_binding(
            &rough_id, "Roughness", rough_default, &mat_node_id, "roughness", 1.0,
        ));
        // One shared "Ambient" fill knob fans out to every material's ambient
        // (a single source_id across all mat_k bindings — the preset_runtime
        // fan-out). Default 0.0 = the lights-only look; raise it for flat fill.
        // The card param itself is pushed once after the loop.
        card_bindings.push(card_binding(
            "scene_ambient", "Ambient", 0.0, &mat_node_id, "ambient", 1.0,
        ));

        // The group's outward interface: the mesh geometry, the material,
        // this object's transform, and (when present) the base-color
        // texture, each exposed through the `system.group_output` and wired
        // out to the shared render node.
        let out_id = fresh_id();
        let mut outputs = vec![
            InterfacePortDef { name: "vertices".to_string(), port_type: "Array(Vertex)".to_string() },
            InterfacePortDef { name: "material".to_string(), port_type: "Material".to_string() },
            InterfacePortDef { name: "transform".to_string(), port_type: "Transform".to_string() },
        ];
        group_wires.push(wire(mesh_id, "vertices", out_id, "vertices"));
        group_wires.push(wire(mat_id, "out", out_id, "material"));

        // Recenter this object at the origin so the fixed-target orbit
        // camera frames the (not-recentered) gltf_mesh_source output — same
        // convention `gltf_mesh_source_renders_azalea_to_png` proves. Lives
        // on this object's OWN `node.transform_3d` now (D9 — the recenter
        // moved off the shared render node onto the per-object atom), no
        // transform sliders on the card by default: transforms are
        // performed via expose-what-you-need, gizmos (P6), or the group
        // face (D6), not a scene-editor slider wall.
        let transform_node_id = format!("transform_{k}");
        let transform_id = fresh_id();
        let mut transform_node = plain_node(
            transform_id,
            &transform_node_id,
            "node.transform_3d",
            &transform_node_id,
        );
        transform_node.params.insert("pos_x".to_string(), float(-center[0]));
        transform_node.params.insert("pos_y".to_string(), float(-center[1]));
        transform_node.params.insert("pos_z".to_string(), float(-center[2]));
        group_nodes.push(transform_node);
        group_wires.push(wire(transform_id, "transform", out_id, "transform"));

        string_bindings.push(StringBindingDef {
            id: MODEL_FILE_PARAM_ID.to_string(),
            label: "Model File".to_string(),
            default_value: path_str.clone(),
            target: BindingTarget::Node {
                node_id: NodeId::new(&mesh_node_id),
                param: "path".to_string(),
            },
        });

        let has_tex = m.base_color_texture.is_some();
        if let Some(tex_index) = m.base_color_texture {
            let tex_node_id = format!("tex_{k}");
            let tex_id = fresh_id();
            let mut tex_node =
                plain_node(tex_id, &tex_node_id, "node.gltf_texture_source", &tex_node_id);
            tex_node
                .params
                .insert("texture_index".to_string(), int(tex_index as i32));
            tex_node.params.insert("color_space".to_string(), enum_val(0)); // sRGB — correct for albedo
            // v1 default — the summary doesn't carry per-texture pixel
            // dimensions yet, so every base-color map resamples to 1024².
            // TODO: thread the source image's actual width/height through
            // `GltfImportSummary`/`GltfMaterialInfo` so a non-1024 texture
            // doesn't resample.
            tex_node.params.insert("width".to_string(), int(1024));
            tex_node.params.insert("height".to_string(), int(1024));
            group_nodes.push(tex_node);

            outputs.push(InterfacePortDef {
                name: "baseColor".to_string(),
                port_type: "Texture2D".to_string(),
            });
            group_wires.push(wire(tex_id, "out", out_id, "baseColor"));

            string_bindings.push(StringBindingDef {
                id: MODEL_FILE_PARAM_ID.to_string(),
                label: "Model File".to_string(),
                default_value: path_str.clone(),
                target: BindingTarget::Node {
                    node_id: NodeId::new(&tex_node_id),
                    param: "path".to_string(),
                },
            });

            textures_wired += 1;
        }

        // `system.group_output` closes the body; its port names are the
        // interface output names the inner wires above target. A boundary node
        // carries no params and no title.
        group_nodes.push(plain_node(
            out_id,
            &format!("object_{k}_out"),
            GROUP_OUTPUT_TYPE_ID,
            "output",
        ));

        // The group box itself, named for the material so the top level reads as
        // labeled boxes a performer can navigate. Folded away at load; only its
        // outputs cross to the top level.
        let group_id = fresh_id();
        let mut group_node =
            plain_node(group_id, &format!("object_{k}"), GROUP_TYPE_ID, &group_name);
        group_node.group = Some(Box::new(GroupDef {
            interface: GroupInterface { inputs: Vec::new(), outputs, params: Vec::new() },
            nodes: group_nodes,
            wires: group_wires,
            // A distinct high-saturation header tint per object so a multi-mesh
            // import reads as a few colour-coded boxes at a glance.
            tint: Some(group_tint(k)),
        }));
        nodes.push(group_node);

        // Top-level wires: the group's outputs feed the shared render node —
        // after flattening these become the exact `mesh_k.vertices → render.mesh_k`
        // (etc.) wires the ungrouped assembler produced.
        wires.push(wire(group_id, "vertices", render_id, &format!("mesh_{k}")));
        wires.push(wire(group_id, "material", render_id, &format!("material_{k}")));
        wires.push(wire(group_id, "transform", render_id, &format!("transform_{k}")));
        if has_tex {
            wires.push(wire(group_id, "baseColor", render_id, &format!("base_color_map_{k}")));
        }
    }

    nodes.push(render_node);

    // Scene-wide atmosphere (fog + god rays): render_scene's `atmosphere`
    // input is lazy — unwired is byte-identical to wired-at-defaults, since
    // every param (fog_density, shaft_intensity, …) defaults to 0 (off).
    // Wired unconditionally, at those same all-off defaults, purely so the
    // God Rays card has a live node to bind — an import looks unchanged
    // until that slider moves.
    let atmosphere_id = fresh_id();
    nodes.push(plain_node(atmosphere_id, "atmosphere", "node.atmosphere", "atmosphere"));

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

    // Depth of field, packaged as the same "dof" node group CinematicScene
    // ships (CINEMATIC_POST P1/P4, BUG-137's coc_dilate fix): coc_from_depth
    // → coc_dilate (fixes the hard cutoff at depth discontinuities) →
    // bokeh_gather occlusion-aware disc gather, reading the shared lens's
    // focus_distance/f_stop.
    let mut dof_nodes: Vec<EffectGraphNode> = Vec::new();
    let mut dof_wires: Vec<EffectGraphWire> = Vec::new();
    let dof_in_id = fresh_id();
    dof_nodes.push(plain_node(dof_in_id, "dof_in", GROUP_INPUT_TYPE_ID, "input"));
    let coc_id = fresh_id();
    let mut coc_node = plain_node(coc_id, "coc", "node.coc_from_depth", "coc");
    coc_node.params.insert("max_radius".to_string(), float(24.0));
    dof_nodes.push(coc_node);
    let coc_dilate_id = fresh_id();
    dof_nodes.push(plain_node(coc_dilate_id, "coc_dilate", "node.coc_dilate", "coc_dilate"));
    let bokeh_id = fresh_id();
    let mut bokeh_node = plain_node(bokeh_id, "bokeh", "node.bokeh_gather", "bokeh");
    bokeh_node.params.insert("max_radius".to_string(), float(24.0));
    dof_nodes.push(bokeh_node);
    let dof_out_id = fresh_id();
    dof_nodes.push(plain_node(dof_out_id, "dof_out", GROUP_OUTPUT_TYPE_ID, "output"));
    dof_wires.push(wire(dof_in_id, "depth", coc_id, "depth"));
    dof_wires.push(wire(dof_in_id, "camera", coc_id, "camera"));
    dof_wires.push(wire(coc_id, "out", coc_dilate_id, "in"));
    dof_wires.push(wire(coc_dilate_id, "out", bokeh_id, "width"));
    dof_wires.push(wire(dof_in_id, "color", bokeh_id, "in"));
    dof_wires.push(wire(bokeh_id, "out", dof_out_id, "out"));

    let dof_group_id = fresh_id();
    let mut dof_group_node = plain_node(dof_group_id, "dof", GROUP_TYPE_ID, "dof");
    dof_group_node.title = Some("Depth of Field".to_string());
    dof_group_node.group = Some(Box::new(GroupDef {
        interface: GroupInterface {
            inputs: vec![
                InterfacePortDef { name: "depth".to_string(), port_type: "Texture2D".to_string() },
                InterfacePortDef { name: "camera".to_string(), port_type: "Camera".to_string() },
                InterfacePortDef { name: "color".to_string(), port_type: "Texture2D".to_string() },
            ],
            outputs: vec![InterfacePortDef { name: "out".to_string(), port_type: "Texture2D".to_string() }],
            params: Vec::new(),
        },
        nodes: dof_nodes,
        wires: dof_wires,
        tint: None,
    }));
    nodes.push(dof_group_node);

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
    // consumer (render_scene, the ao/dof groups) — the node whose
    // focus_distance/f_stop the Depth of Field card reads.
    wires.push(wire(camera_id, "out", lens_id, "camera"));
    wires.push(wire(lens_id, "out", render_id, "camera"));
    wires.push(wire(envmap_id, "envmap", render_id, "envmap"));
    wires.push(wire(sun_id, "out", render_id, "light_0"));
    wires.push(wire(atmosphere_id, "atmosphere", render_id, "atmosphere"));

    // render_scene → ao (contact AO) → dof (defocus) → final.
    wires.push(wire(render_id, "depth", ao_group_id, "depth"));
    wires.push(wire(lens_id, "out", ao_group_id, "camera"));
    wires.push(wire(render_id, "color", ao_group_id, "color"));
    wires.push(wire(render_id, "depth", dof_group_id, "depth"));
    wires.push(wire(lens_id, "out", dof_group_id, "camera"));
    wires.push(wire(ao_group_id, "out", dof_group_id, "color"));
    wires.push(wire(dof_group_id, "out", final_id, "in"));

    // Shared framing / light / environment card knobs. These come LAST in
    // `card_params` (after the per-object material knobs) but read first on
    // the card as the primary performance controls. Angle sliders store
    // RADIANS and carry `is_angle` (the app-wide convention — the slider
    // shows/edits degrees, storage is radians), so the binding is a
    // pass-through (`scale = 1.0`); mixing a degrees store with an `is_angle`
    // formatter double-converts (40° → 2298°). Defaults mirror the
    // `camera`/`sun`/`envmap` node params set above so the card is a faithful
    // mirror of the assembled look.
    card_params.push(card_param(
        "cam_orbit", "Camera Orbit", -180.0 * DEG2RAD, 180.0 * DEG2RAD, 0.7, true, "Camera",
    )); // angle (radians)
    card_bindings.push(card_binding(
        "cam_orbit", "Camera Orbit", 0.7, "camera", "orbit", 1.0,
    ));
    card_params.push(card_param(
        "cam_tilt", "Camera Tilt", -89.0 * DEG2RAD, 89.0 * DEG2RAD, 0.3, true, "Camera",
    )); // angle (radians)
    card_bindings.push(card_binding(
        "cam_tilt", "Camera Tilt", 0.3, "camera", "tilt", 1.0,
    ));
    card_params.push(card_param(
        "cam_dist",
        "Camera Distance",
        0.1,
        (distance * 4.0).max(1.0),
        distance,
        false,
        "Camera",
    ));
    card_bindings.push(card_binding(
        "cam_dist", "Camera Distance", distance, "camera", "distance", 1.0,
    ));
    card_params.push(card_param(
        "cam_fov", "Camera FOV", 20.0 * DEG2RAD, 120.0 * DEG2RAD, 0.9, true, "Camera",
    )); // angle (radians)
    card_bindings.push(card_binding(
        "cam_fov", "Camera FOV", 0.9, "camera", "fov_y", 1.0,
    ));

    card_params.push(card_param("sun_int", "Sun Intensity", 0.0, 10.0, 3.5, false, "Sun"));
    card_bindings.push(card_binding("sun_int", "Sun Intensity", 3.5, "sun", "intensity", 1.0));
    card_params.push(card_param("sun_x", "Sun X", -15.0, 15.0, 5.0, false, "Sun"));
    card_bindings.push(card_binding("sun_x", "Sun X", 5.0, "sun", "pos_x", 1.0));
    card_params.push(card_param("sun_y", "Sun Y", -15.0, 15.0, 2.0, false, "Sun"));
    card_bindings.push(card_binding("sun_y", "Sun Y", 2.0, "sun", "pos_y", 1.0));
    card_params.push(card_param("sun_z", "Sun Z", -15.0, 15.0, 3.0, false, "Sun"));
    card_bindings.push(card_binding("sun_z", "Sun Z", 3.0, "sun", "pos_z", 1.0));
    // Shadow tier on the outer card: crisp-vs-soft is a look decision, so it
    // rides next to the sun sliders instead of requiring a trip into the
    // graph. Labels must stay in `ShadowSoftness` variant order — the
    // EnumRound binding writes the label's index straight into the light's
    // `shadow_softness` enum. Default 0 (Hard) mirrors the crisp-shadow
    // default the assembler stamps on the sun node above.
    let mut shadow_param = card_param("sun_shadow", "Shadow Type", 0.0, 3.0, 0.0, false, "Sun");
    shadow_param.whole_numbers = true;
    shadow_param.value_labels = vec![
        "Hard".to_string(),
        "Soft".to_string(),
        "Very Soft".to_string(),
        "Contact".to_string(),
    ];
    card_params.push(shadow_param);
    let mut shadow_binding =
        card_binding("sun_shadow", "Shadow Type", 0.0, "sun", "shadow_softness", 1.0);
    shadow_binding.convert = manifold_core::effects::ParamConvert::EnumRound;
    card_bindings.push(shadow_binding);

    // `node.bake_environment`'s `intensity` master scales the WHOLE baked map
    // (every studio term), so 0 = a black environment and no image-based
    // lighting at all — unlike `horizon_strength`, which only dims the horizon
    // band and leaves the bright zenith/sun terms lighting the scene. Default 0
    // gives the lights-only look; raise it for a reflective studio environment.
    card_params.push(card_param("env_intensity", "Environment", 0.0, 4.0, 0.0, false, "Environment"));
    card_bindings.push(card_binding(
        "env_intensity", "Environment", 0.0, "envmap", "intensity", 1.0,
    ));
    // The shared Ambient fill knob (its per-material bindings were fanned out
    // in the object loop above). 0.0 = no flat fill (lights-only); raise it to
    // lift the shadow side of every material at once.
    card_params.push(card_param("scene_ambient", "Ambient", 0.0, 1.0, 0.0, false, "Environment"));

    // SSAO — contact-occlusion arm. These bind to the ssao atom's own params
    // (plain params, not port-shadowed), so they apply their defaults cleanly.
    // `ssao_radius` is scene-scaled (world units) to the model's bounding
    // radius; the rest mirror the atom's defaults.
    card_params.push(card_param(
        "ssao_intensity", "SSAO Intensity", 0.0, 4.0, 1.0, false, "Ambient Occlusion",
    ));
    card_bindings.push(card_binding(
        "ssao_intensity", "SSAO Intensity", 1.0, "ssao", "intensity", 1.0,
    ));
    card_params.push(card_param(
        "ssao_radius", "SSAO Radius", 0.01, 5.0, ssao_radius_default, false, "Ambient Occlusion",
    ));
    card_bindings.push(card_binding(
        "ssao_radius", "SSAO Radius", ssao_radius_default, "ssao", "radius", 1.0,
    ));
    // No `ssao_bias` card — node.ssao_gtao (CINEMATIC_POST D9) has no `bias`
    // param; the per-tap range check subsumes it.

    // Depth of field — both bind the shared "lens" node (CINEMATIC_POST D6).
    // Focus distance is scene-scaled like the camera distance card, defaulting
    // to it, so the subject starts in focus; range widens with `distance` the
    // same way `cam_dist` does so a huge or tiny import both stay editable.
    // F-Stop defaults to the lens's own neutral top-of-range (32, no visible
    // blur) — dialing it down is what turns DoF on.
    let focus_distance_max = (distance * 4.0).max(1.0);
    card_params.push(card_param(
        "dof_focus", "Focus Distance", 0.1, focus_distance_max, distance, false, "Depth of Field",
    ));
    card_bindings.push(card_binding(
        "dof_focus", "Focus Distance", distance, "lens", "focus_distance", 1.0,
    ));
    card_params.push(card_param("dof_fstop", "F-Stop", 0.5, 32.0, 32.0, false, "Depth of Field"));
    card_bindings.push(card_binding("dof_fstop", "F-Stop", 32.0, "lens", "f_stop", 1.0));

    // Atmosphere — two independent node.atmosphere knobs, each its own
    // slider (not folded together): `shaft_intensity` alone does nothing
    // visible — the shaft march scatters light through the fog medium, so
    // `fog_density` must also be raised for beams to actually appear
    // (docs/VOLUMETRIC_LIGHT_DESIGN.md D1/D4: "the two faders", both
    // required together). Kept separate rather than one combined "God
    // Rays" knob so fog and shafts stay independently dialable, matching
    // how every other card knob here maps 1:1 to a node param. Both
    // default to 0 (off, matches the atom's own defaults) — the sun's
    // `cast_shadows` (already on above) is what gives the shafts their
    // shape once both faders are up.
    //
    // Fog density is scene-scaled like `ssao_radius`/`dof_focus` (BUG-149):
    // the atom's `fog_density` is per-world-unit, so a raw 0–1 slider is a
    // cliff on any real import (0.13 at the apricot fixture's 27.87-unit
    // framing distance is optical depth ~3.6 ≈ 97% fog — flat grey mesh,
    // and the shaft march's in-scattering then blows out the whole frame).
    // The binding scale maps the slider to optical depth AT THE SUBJECT
    // (density · framing distance): 3.0/distance puts slider 1.0 at depth
    // 3 ≈ 95% fogged (whiteout stays reachable), 0.5 ≈ 78%, 0.1 ≈ 26%
    // haze — the same perceptual fader on any model scale.
    card_params.push(card_param("fog_density", "Fog Density", 0.0, 1.0, 0.0, false, "Atmosphere"));
    card_bindings.push(card_binding(
        "fog_density", "Fog Density", 0.0, "atmosphere", "fog_density", 3.0 / distance,
    ));
    card_params.push(card_param("god_rays", "God Rays", 0.0, 2.0, 0.0, false, "Atmosphere"));
    card_bindings.push(card_binding(
        "god_rays", "God Rays", 0.0, "atmosphere", "shaft_intensity", 1.0,
    ));

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
        string_params: vec![StringParamSpecDef {
            id: MODEL_FILE_PARAM_ID.to_string(),
            name: "Model File".to_string(),
            default_value: path_str,
            is_file_picker: true,
            use_dropdown: false,
        }],
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

    let report = ImportReport {
        material_count: summary.materials.len(),
        object_count: n,
        textures_wired,
        dropped_over_cap,
        default_material_vertex_count: summary.default_material_vertex_count,
        camera_synthesized: true,
    };

    Ok((def, report))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::node_graph::PrimitiveRegistry;
    use crate::preset_runtime::PresetRuntime;
    use manifold_core::effect_graph_def::BindingTarget;

    fn azalea_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/cc0__oomurasaki_azalea_r._x_pulchrum.glb")
    }

    /// CPU-only, fast — not gated behind `#[ignore]`. Guards the
    /// file-missing case (no fixture in a checkout without
    /// `tests/fixtures/gltf/`) so CI without the large fixture still
    /// passes; when the fixture IS present, asserts the assembled graph's
    /// known azalea shape.
    #[test]
    fn assembles_azalea_into_two_object_render_scene_graph() {
        let path = azalea_fixture_path();
        if !path.exists() {
            println!(
                "assembles_azalea_into_two_object_render_scene_graph: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble azalea");
        println!("azalea import report: {report:?}");

        assert_eq!(report.material_count, 2, "azalea has 2 materials with geometry");
        assert_eq!(report.object_count, 2);
        assert_eq!(report.textures_wired, 2, "both azalea materials carry a base-color texture");
        assert_eq!(report.dropped_over_cap, 0);
        assert_eq!(report.default_material_vertex_count, 0);
        assert!(report.camera_synthesized);

        assert!(
            def.nodes.iter().any(|n| n.type_id == GENERATOR_INPUT_TYPE_ID),
            "assembled graph must carry a system.generator_input node"
        );
        assert!(
            def.nodes.iter().any(|n| n.type_id == FINAL_OUTPUT_TYPE_ID),
            "assembled graph must carry a system.final_output node"
        );

        let meta = def.preset_metadata.as_ref().expect("v2 metadata");
        assert_eq!(meta.string_params.len(), 1, "exactly one model_file string param");
        assert_eq!(meta.string_params[0].id, "model_file");
        assert!(meta.string_params[0].is_file_picker);

        assert_eq!(meta.string_bindings.len(), 4, "2 mesh + 2 texture path bindings");
        for b in &meta.string_bindings {
            assert_eq!(b.id, "model_file");
            match &b.target {
                BindingTarget::Node { param, .. } => assert_eq!(param, "path"),
                other => panic!("expected a Node binding target, got {other:?}"),
            }
        }

        // Curated performance surface. Azalea has 2 objects → 4 camera + 5 sun
        // + 1 Environment + 1 Ambient + 2×(metallic + roughness) = 15
        // framing/material sliders, PLUS 2 GTAO knobs (radius, intensity --
        // `bias` dropped, CINEMATIC_POST D9(b)) + 2 DoF (focus, f-stop) +
        // 2 Atmosphere (fog density, god rays — no Motion Blur section,
        // BUG-136) = 21.
        assert_eq!(
            meta.params.len(),
            21,
            "15 framing/material + 2 GTAO + 2 DoF + 2 atmosphere (fog + god rays)"
        );
        // Every param routes one-to-one except the shared Ambient, which fans
        // out to every material's ambient (2 for azalea). 21 + 1 = 22.
        assert_eq!(
            meta.bindings.len(),
            22,
            "21 params, Ambient fanned to 2 materials"
        );
        // Every card param routes to at least one node param.
        for p in &meta.params {
            assert!(
                meta.bindings.iter().any(|b| b.id == p.id),
                "card param `{}` has no binding",
                p.id
            );
        }
        // Every binding must reference a param that actually exists (address
        // by id, never by position — the fan-out rule).
        for b in &meta.bindings {
            assert!(
                meta.params.iter().any(|p| p.id == b.id),
                "binding `{}` has no matching param",
                b.id
            );
            assert!(
                matches!(b.target, BindingTarget::Node { .. }),
                "import card bindings route to inner nodes"
            );
        }
        // Spot-check the shared framing knobs and the per-object material
        // knobs. No more numeric label suffix — the `section` (D5/D9) now
        // carries the per-object identity instead.
        for id in ["cam_orbit", "cam_dist", "sun_int", "env_intensity", "scene_ambient", "metal_0", "rough_1"] {
            assert!(meta.params.iter().any(|p| p.id == id), "missing card param `{id}`");
        }
        let metal0 = meta.params.iter().find(|p| p.id == "metal_0").unwrap();
        assert_eq!(metal0.name, "Metallic", "no ' 1'-style suffix — section carries the identity now");
        assert!(metal0.section.is_some(), "per-object knob carries a section");
        let rough1 = meta.params.iter().find(|p| p.id == "rough_1").unwrap();
        assert_eq!(rough1.name, "Roughness");
        assert_ne!(
            metal0.section, rough1.section,
            "the two azalea objects get distinct sections (their own group names)"
        );
        // Shared framing/light/environment knobs carry the fixed section names.
        let cam_orbit = meta.params.iter().find(|p| p.id == "cam_orbit").unwrap();
        assert_eq!(cam_orbit.section.as_deref(), Some("Camera"));
        let sun_int = meta.params.iter().find(|p| p.id == "sun_int").unwrap();
        assert_eq!(sun_int.section.as_deref(), Some("Sun"));
        let env_intensity = meta.params.iter().find(|p| p.id == "env_intensity").unwrap();
        assert_eq!(env_intensity.section.as_deref(), Some("Environment"));
        // Lights-only defaults: the environment master and the shared ambient
        // fill both start at 0, so a freshly imported model is lit purely by
        // its scene lights.
        assert_eq!(env_intensity.default_value, 0.0, "environment bakes dark by default");
        let scene_ambient = meta.params.iter().find(|p| p.id == "scene_ambient").unwrap();
        assert_eq!(scene_ambient.default_value, 0.0, "no ambient fill by default");
        // The Ambient card fans out to every material's `ambient` param.
        let ambient_targets: std::collections::HashSet<String> = meta
            .bindings
            .iter()
            .filter(|b| b.id == "scene_ambient")
            .filter_map(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    assert_eq!(param, "ambient");
                    Some(node_id.as_str().to_string())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            ambient_targets,
            ["mat_0", "mat_1"].into_iter().map(String::from).collect(),
            "shared Ambient drives both azalea materials"
        );
        // The environment master routes to the envmap's new `intensity` param
        // (not the old horizon_strength).
        let env_binding = meta.bindings.iter().find(|b| b.id == "env_intensity").unwrap();
        match &env_binding.target {
            BindingTarget::Node { node_id, param } => {
                assert_eq!(node_id.as_str(), "envmap");
                assert_eq!(param, "intensity");
            }
            other => panic!("expected envmap node target, got {other:?}"),
        }

        // Camera angle sliders store radians (app-wide `is_angle` convention),
        // so the binding is a pass-through — no unit fold at the write boundary.
        assert!(cam_orbit.is_angle, "camera orbit slider is an angle param");
        assert!(
            (cam_orbit.default_value - 0.7).abs() < 1e-6,
            "camera orbit default is stored in radians"
        );
        let orbit = meta.bindings.iter().find(|b| b.id == "cam_orbit").unwrap();
        assert!(
            (orbit.scale - 1.0).abs() < 1e-6,
            "camera angle bindings pass radians straight through"
        );

        // GTAO, the lens, and DoF's atom chain are wired into the spine.
        // No motion blur (BUG-136 + fusion cost, see the removal comment
        // in `build_import_graph`) and atmosphere is a top-level node.
        for present in [
            "node.ssao_gtao",
            "node.bilateral_blur",
            "node.camera_lens",
            "node.coc_from_depth",
            "node.coc_dilate",
            "node.bokeh_gather",
            "node.atmosphere",
        ] {
            assert!(
                def.nodes.iter().any(|n| n.type_id == present)
                    || def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                        g.nodes.iter().any(|inner| inner.type_id == present)
                    }),
                "imported graph must carry `{present}`"
            );
        }
        // `node.motion_blur` was removed (BUG-136: no visible effect live,
        // despite correct wiring — see the removal comment above) and
        // `node.variable_blur` was P4's superseded DoF blur stage — neither
        // is reintroduced.
        for absent in ["node.motion_blur", "node.variable_blur"] {
            assert!(
                !def.nodes.iter().any(|n| n.type_id == absent),
                "`{absent}` should not be in the imported graph"
            );
        }
        // The GTAO knobs are exposed and still bind the ssao node directly —
        // grouping doesn't change its explicit stable node_id.
        for id in ["ssao_intensity", "ssao_radius"] {
            let p = meta.params.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("missing SSAO card param `{id}`"));
            assert_eq!(p.section.as_deref(), Some("Ambient Occlusion"), "`{id}` is an AO knob");
            let b = meta.bindings.iter().find(|b| b.id == id).unwrap();
            match &b.target {
                BindingTarget::Node { node_id, .. } => {
                    assert_eq!(node_id.as_str(), "ssao", "`{id}` binds the ssao node")
                }
                other => panic!("expected a Node target for `{id}`, got {other:?}"),
            }
        }
        // New DoF / atmosphere knobs, each targeting its own node. Fog
        // density and god rays are two independent sliders, not folded
        // into one — the atom needs both raised together to show beams
        // (see the card-authoring comment in `build_import_graph`), but
        // each still routes to its own atmosphere param 1:1 like every
        // other card knob.
        for (id, section, target_node) in [
            ("dof_focus", "Depth of Field", "lens"),
            ("dof_fstop", "Depth of Field", "lens"),
            ("fog_density", "Atmosphere", "atmosphere"),
            ("god_rays", "Atmosphere", "atmosphere"),
        ] {
            let p = meta.params.iter().find(|p| p.id == id).unwrap_or_else(|| panic!("missing card param `{id}`"));
            assert_eq!(p.section.as_deref(), Some(section), "`{id}` section");
            let b = meta.bindings.iter().find(|b| b.id == id).unwrap();
            match &b.target {
                BindingTarget::Node { node_id, .. } => {
                    assert_eq!(node_id.as_str(), target_node, "`{id}` binds `{target_node}`")
                }
                other => panic!("expected a Node target for `{id}`, got {other:?}"),
            }
        }
        // Defaults keep a fresh import visually unchanged: DoF/fog/god rays
        // all start at their neutral (no-op) value.
        let dof_fstop = meta.params.iter().find(|p| p.id == "dof_fstop").unwrap();
        assert_eq!(dof_fstop.default_value, 32.0, "f-stop starts at the lens's no-blur top-of-range");
        let fog_density = meta.params.iter().find(|p| p.id == "fog_density").unwrap();
        assert_eq!(fog_density.default_value, 0.0, "fog starts off");
        let god_rays = meta.params.iter().find(|p| p.id == "god_rays").unwrap();
        assert_eq!(god_rays.default_value, 0.0, "god rays start off");
        // BUG-149: fog density is scene-scaled — the binding maps the 0–1
        // slider to optical depth at the subject (3.0 / framing distance,
        // where the framing distance is exactly the cam_dist card default),
        // never raw per-world-unit density.
        let cam_dist = meta.params.iter().find(|p| p.id == "cam_dist").unwrap();
        let fog_binding = meta.bindings.iter().find(|b| b.id == "fog_density").unwrap();
        assert!(
            (fog_binding.scale * cam_dist.default_value - 3.0).abs() < 1e-4,
            "fog slider must scale by 3.0/framing-distance (got scale {} at distance {})",
            fog_binding.scale,
            cam_dist.default_value
        );
        for gone in [
            "lens_focus", "lens_fstop", "lens_shutter", "lens_ev", "dof_radius",
            "motion_blur_px", "mb_shutter", "ssao_bias",
        ] {
            assert!(
                !meta.params.iter().any(|p| p.id == gone),
                "unused card param id `{gone}` should not exist"
            );
        }
        // Shadow type defaults to Hard (enum 0) for the crisp dramatic look.
        let sun_shadow = meta.params.iter().find(|p| p.id == "sun_shadow").unwrap();
        assert_eq!(sun_shadow.default_value, 0.0, "shadow type defaults to Hard");
    }

    /// Structural gate (fast, no GPU): the assembled azalea graph must
    /// compile through the real `PrimitiveRegistry` — every node type_id
    /// resolves, every wire's ports exist and type-check, both boundary
    /// nodes are present and wired correctly. Catches a bad port/param
    /// name or a missing wire before the (slow, `#[ignore]`d) GPU proof.
    #[test]
    fn assembled_azalea_graph_passes_from_def_structural_check() {
        let path = azalea_fixture_path();
        if !path.exists() {
            println!(
                "assembled_azalea_graph_passes_from_def_structural_check: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, _report) = assemble_import_graph(&path).expect("assemble azalea");
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None)
            .expect("assembled azalea graph must compile through PresetRuntime::from_def");
    }

    /// Grouping proof, fixture-free: each object's producers must live inside one
    /// named, tinted group, and the grouped graph must flatten to the SAME flat
    /// wiring the ungrouped assembler produced (compared in node_id space) with
    /// every card/string binding still resolving. Uses a synthetic two-material
    /// summary — one textured, one not — so it needs no `.glb` on disk.
    #[test]
    fn build_import_graph_groups_each_object_and_flattens_to_flat_wiring() {
        use super::gltf_load::GltfMaterialInfo;
        use manifold_core::effect_graph_def::GROUP_TYPE_ID;
        use manifold_core::flatten::flatten_groups;

        let mat = |material_index: u32, name: &str, verts: u32, tex: Option<u32>| GltfMaterialInfo {
            material_index,
            name: Some(name.to_string()),
            base_color_factor: [0.5, 0.5, 0.5, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            emissive: [0.0, 0.0, 0.0],
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: tex,
            vertex_count: verts,
        };
        let summary = GltfImportSummary {
            // Largest-vertex-first sort makes object 0 = Leaf (textured), 1 = Bark.
            materials: vec![mat(0, "Leaf", 1200, Some(0)), mat(1, "Bark", 800, None)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_model.glb");
        let (def, report) = build_import_graph(&summary, path).expect("build grouped graph");
        assert_eq!(report.object_count, 2);
        assert_eq!(report.textures_wired, 1);

        // Top level: two per-object group boxes PLUS the "ao" and "dof"
        // presentation groups (CINEMATIC_POST), no bare producer nodes.
        let groups: Vec<_> = def.nodes.iter().filter(|n| n.type_id == GROUP_TYPE_ID).collect();
        assert_eq!(groups.len(), 4, "2 object groups + ao + dof");
        assert!(groups.iter().all(|g| g.group.is_some()));
        for bare in [
            "node.gltf_mesh_source",
            "node.pbr_material",
            "node.gltf_texture_source",
            "node.transform_3d",
        ] {
            assert!(
                !def.nodes.iter().any(|n| n.type_id == bare),
                "producer `{bare}` must live inside a group, not at the top level"
            );
        }
        // Only the per-object groups carry a tint (CINEMATIC_POST's ao/dof
        // groups are untinted presentation boxes, not per-object identity).
        let object_groups: Vec<_> = groups
            .iter()
            .filter(|g| g.group.as_ref().unwrap().tint.is_some())
            .copied()
            .collect();
        assert_eq!(object_groups.len(), 2, "one tinted group per object");
        // Every object group's interface declares a `transform` output (D9).
        for g in &object_groups {
            let outputs = &g.group.as_ref().unwrap().interface.outputs;
            assert!(
                outputs.iter().any(|o| o.name == "transform" && o.port_type == "Transform"),
                "every object group exposes a transform output"
            );
        }
        // Distinct tints per object group (legibility).
        let tints: Vec<_> = object_groups.iter().filter_map(|g| g.group.as_ref().unwrap().tint).collect();
        assert_eq!(tints.len(), 2, "every object group gets a tint");
        assert_ne!(tints[0], tints[1], "each object group gets its own tint");

        // Flatten and prove the runtime sees the same flat wiring the ungrouped
        // assembler produced — in node_id space (survives id renumbering + handle
        // prefixing).
        let flat = flatten_groups(&def).expect("grouped import graph flattens");
        let id_of = |doc_id: u32| -> String {
            flat.nodes
                .iter()
                .find(|n| n.id == doc_id)
                .map(|n| n.node_id.as_str().to_string())
                .unwrap_or_default()
        };
        let conn: std::collections::HashSet<(String, String, String, String)> = flat
            .wires
            .iter()
            .map(|w| (id_of(w.from_node), w.from_port.clone(), id_of(w.to_node), w.to_port.clone()))
            .collect();
        for (from_id, from_port, to_port) in [
            ("mesh_0", "vertices", "mesh_0"),
            ("mat_0", "out", "material_0"),
            ("tex_0", "out", "base_color_map_0"),
            ("mesh_1", "vertices", "mesh_1"),
            ("mat_1", "out", "material_1"),
            ("transform_0", "transform", "transform_0"),
            ("transform_1", "transform", "transform_1"),
        ] {
            assert!(
                conn.contains(&(
                    from_id.to_string(),
                    from_port.to_string(),
                    "render".to_string(),
                    to_port.to_string(),
                )),
                "flattened graph missing wire {from_id}.{from_port} -> render.{to_port}"
            );
        }
        // Bark has no texture — no base_color_map_1 wire.
        assert!(
            !conn.iter().any(|(_, _, _, tp)| tp == "base_color_map_1"),
            "untextured object must not wire a base-color map"
        );
        // No group / boundary nodes survive flattening.
        assert!(
            !flat.nodes.iter().any(|n| {
                n.type_id == GROUP_TYPE_ID || n.type_id.contains("group_output") || n.type_id.contains("group_input")
            }),
            "flattened graph must contain no group or boundary nodes"
        );

        // Every card + string binding still targets a node_id that exists post-flatten.
        let meta = def.preset_metadata.as_ref().expect("v2 metadata");
        let flat_ids: std::collections::HashSet<&str> =
            flat.nodes.iter().map(|n| n.node_id.as_str()).collect();
        for b in &meta.bindings {
            if let BindingTarget::Node { node_id, .. } = &b.target {
                assert!(
                    flat_ids.contains(node_id.as_str()),
                    "card binding `{}` targets `{}`, gone after flatten",
                    b.id,
                    node_id.as_str()
                );
            }
        }
        for b in &meta.string_bindings {
            if let BindingTarget::Node { node_id, .. } = &b.target {
                assert!(
                    flat_ids.contains(node_id.as_str()),
                    "string binding targets `{}`, gone after flatten",
                    node_id.as_str()
                );
            }
        }

        // The editor's own data path (`GraphSnapshot::from_def`, which routes a
        // grouped def through the group-preserving structural snapshot) must show
        // all four groups as navigable boxes — each carrying its inner producers —
        // not a flat wall of nodes. This is the legibility payoff, verified at the
        // snapshot layer (the pixels still want Peter's eyes on a real model).
        let snap = crate::node_graph::GraphSnapshot::from_def(&def)
            .expect("editor snapshot builds from the grouped def");
        let snap_groups: Vec<_> =
            snap.nodes.iter().filter(|n| n.group.is_some()).collect();
        assert_eq!(snap_groups.len(), 4, "editor snapshot shows 2 object + ao + dof group boxes");
        let snap_object_groups: Vec<_> = snap_groups
            .iter()
            .filter(|g| g.group.as_ref().unwrap().nodes.iter().any(|inner| inner.type_id == "node.pbr_material"))
            .collect();
        assert_eq!(snap_object_groups.len(), 2, "exactly the two object groups carry a material node");

        // Finally, it must build through the production loader (which flattens).
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None)
            .expect("grouped import graph must build through PresetRuntime::from_def");
    }

    /// GRAPH_TOOLING_DESIGN D6: `assemble_import_graph`'s output must be
    /// validated through `validate_def` before it reaches the project — the
    /// assembler is code and has bugs. This proves the mechanism the
    /// `manifold-app` importer hook relies on: a deliberately corrupted
    /// assembler-style def (one node's `type_id` rewritten to a type the
    /// registry doesn't know) fails `validate_def` with an issue naming that
    /// node, never silently. Fixture-free — reuses the synthetic two-material
    /// summary from `build_import_graph_groups_each_object_and_flattens_to_flat_wiring`.
    #[test]
    fn corrupted_assembler_output_fails_validation_naming_the_node() {
        use super::gltf_load::GltfMaterialInfo;
        use crate::node_graph::{ValidateKind, validate_def};
        use manifold_gpu::GpuDevice;

        let mat = |material_index: u32, name: &str, verts: u32, tex: Option<u32>| GltfMaterialInfo {
            material_index,
            name: Some(name.to_string()),
            base_color_factor: [0.5, 0.5, 0.5, 1.0],
            metallic: 0.0,
            roughness: 0.6,
            emissive: [0.0, 0.0, 0.0],
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: tex,
            vertex_count: verts,
        };
        let summary = GltfImportSummary {
            materials: vec![mat(0, "Leaf", 1200, Some(0)), mat(1, "Bark", 800, None)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_model.glb");
        let (mut def, _report) = build_import_graph(&summary, path).expect("build graph");

        // Corrupt exactly one node's type_id to something the registry has
        // never heard of — the "assembler wrote a typo" failure class.
        // The producer nodes live inside a group's body (see
        // `build_import_graph_groups_each_object_and_flattens_to_flat_wiring`
        // above), so search recursively rather than only the top level.
        fn find_pbr_material_mut(
            nodes: &mut [manifold_core::effect_graph_def::EffectGraphNode],
        ) -> Option<&mut manifold_core::effect_graph_def::EffectGraphNode> {
            for n in nodes {
                if n.type_id == "node.pbr_material" {
                    return Some(n);
                }
                if let Some(group) = n.group.as_mut()
                    && let Some(found) = find_pbr_material_mut(&mut group.nodes)
                {
                    return Some(found);
                }
            }
            None
        }
        const CORRUPT_TYPE_ID: &str = "node.definitely_not_a_real_type";
        {
            let target = find_pbr_material_mut(&mut def.nodes)
                .expect("assembled graph has a pbr_material node to corrupt");
            target.type_id = CORRUPT_TYPE_ID.to_string();
        }

        let registry = PrimitiveRegistry::with_builtin();
        let device = std::sync::Arc::new(GpuDevice::new());
        let report = validate_def(&def, &registry, ValidateKind::Generator, &device);

        assert!(
            !report.is_valid(),
            "a def with an unknown type_id must fail validate_def, not pass silently"
        );
        // Match by type_id, not doc_id: `validate_def` flattens groups before
        // classifying (this def's producers live inside a group), and
        // flattening renumbers doc ids via a fresh `IdAlloc` — the corrupted
        // node's ORIGINAL id doesn't survive to the error, but its (equally
        // corrupted) type_id does, and the reported node_id still names a
        // real node in the flattened def the error is about.
        assert!(
            report
                .errors
                .iter()
                .any(|issue| issue.type_id.as_deref() == Some(CORRUPT_TYPE_ID) && issue.node_id.is_some()),
            "expected an error naming a node with the corrupted type_id; got: {:?}",
            report.errors
        );
    }

    /// Regression for the glTF-import "unknown parameter 'pos_x_N'" load
    /// failure (Peter, 2026-07-05), REWRITTEN for
    /// SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §2 D3/P2 — the original
    /// subject (a per-object param that only existed once `render_scene`
    /// reconfigured to a higher object count) no longer exists: per-object
    /// TRS is a `transform_n: Transform` PORT now, not a param. The
    /// analogous regression is a PORT that only exists once `render_scene`
    /// reconfigures — `transform_2`, which a naive loader could reject as
    /// "unknown port" if it validated wires against the default 2-object
    /// port surface instead of the reconfigured one. A model with >2
    /// distinct materials (`objects >= 3`) is exactly the shape that used to
    /// trip this; the azalea fixture has only 2 objects, so it never
    /// exercised it — the coverage gap that let the original bug ship. This
    /// synthetic 3-object def reproduces the shape with no large fixture and
    /// must load + wire clean, proving reconfigure runs before wire/port
    /// validation for the new port-based surface too.
    #[test]
    fn render_scene_with_three_objects_loads_transform_nodes() {
        use crate::node_graph::persistence::EffectGraphDefExt;

        let mut render = plain_node(0, "render", "node.render_scene", "render");
        render.params.insert("objects".to_string(), int(3));
        render.params.insert("lights".to_string(), int(1));

        // The port that was unresolvable before the fix: object index 2's
        // transform, which only exists once render_scene reconfigures to
        // objects >= 3.
        let mut transform_2 = plain_node(1, "transform_2", "node.transform_3d", "transform_2");
        transform_2.params.insert("pos_x".to_string(), float(-1.5));
        transform_2.params.insert("pos_y".to_string(), float(0.25));

        let def = EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes: vec![render, transform_2],
            wires: vec![wire(1, "transform", 0, "transform_2")],
        };

        // Validate at the `into_graph` layer — the exact place the
        // "unknown parameter 'pos_x_2'" error was raised for the old shape.
        // (A full `from_def` additionally enforces generator-boundary
        // wiring, which this minimal two-node def deliberately omits — out
        // of scope for the port-surface regression.)
        let registry = PrimitiveRegistry::with_builtin();
        let graph = def.into_graph(&registry).expect(
            "render_scene with objects=3 must accept a transform_2 wire at load \
             (reconfigure runs before port validation)",
        );
        assert!(
            graph.wires().iter().any(|w| w.to.1 == "transform_2"),
            "the transform_2 wire survives into the built graph"
        );
    }

    /// Real held-out proof of Peter's exact case: point `MESH_SNAP_GLB` at a
    /// `.glb` with >2 distinct materials (e.g. the japanese-apricot fixture)
    /// and confirm the assembled generator loads through the production
    /// `PresetRuntime::from_def` without the `unknown parameter 'pos_x_2'`
    /// failure. `#[ignore]` + env-gated: the large held-out fixtures aren't in
    /// the tree, and this is a targeted manual gate, not CI.
    #[test]
    #[ignore = "env-gated held-out glTF; set MESH_SNAP_GLB to a >2-material .glb"]
    fn held_out_gltf_generator_loads_through_from_def() {
        let Ok(glb) = std::env::var("MESH_SNAP_GLB") else {
            println!("MESH_SNAP_GLB unset — skipping");
            return;
        };
        let (def, report) = assemble_import_graph(std::path::Path::new(&glb))
            .unwrap_or_else(|e| panic!("assemble {glb}: {e}"));
        println!("held-out import report: {report:?}");
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None).unwrap_or_else(|e| {
            panic!("held-out glTF generator failed to load through from_def: {e}")
        });
        println!("held-out glTF generator loaded clean ({} objects)", report.object_count);
    }

    // ========================================================================
    // Visual proof — render the assembled azalea graph through the real
    // production path (PresetRuntime::from_def_with_device + render()) and
    // confirm it's actually lit/textured, not just structurally valid.
    // ========================================================================

    /// Decode one IEEE-754 binary16 value to f32 (no `half` dependency).
    /// Copied from `mesh_snapshot.rs` (test-only, small, not worth a shared
    /// module for two call sites).
    #[cfg(feature = "gpu-proofs")]
    fn half_to_f32(h: u16) -> f32 {
        let sign = if (h >> 15) & 1 == 1 { -1.0f32 } else { 1.0f32 };
        let exp = (h >> 10) & 0x1f;
        let mant = h & 0x3ff;
        let mag = if exp == 0 {
            (mant as f32) * 2f32.powi(-24)
        } else if exp == 0x1f {
            if mant == 0 { f32::INFINITY } else { f32::NAN }
        } else {
            (1.0 + (mant as f32) / 1024.0) * 2f32.powi(exp as i32 - 15)
        };
        sign * mag
    }

    #[cfg(feature = "gpu-proofs")]
    /// Reinhard-tonemap an HDR channel to 8-bit: `out = (v/(1+v)).clamp(0,1)*255`.
    fn tonemap_channel(v: f32) -> u8 {
        let ldr = (v / (1.0 + v)).clamp(0.0, 1.0);
        (ldr * 255.0).round() as u8
    }

    #[cfg(feature = "gpu-proofs")]
    /// Render the assembled azalea graph through the PRODUCTION preset
    /// runtime (`PresetRuntime::from_def_with_device` + `render()`), the
    /// same path a real imported `.manifold` project's generator card
    /// takes — not the raw `Graph`/`Executor` harness `mesh_snapshot.rs`
    /// uses for its own lower-level proofs. Both `node.gltf_mesh_source`
    /// (2 objects) and `node.gltf_texture_source` (2 textures) parse on
    /// background threads, so the render is polled (bounded, with a short
    /// sleep between attempts) until the frame is actually non-black,
    /// mirroring `gltf_textured_azalea_renders_through_render_scene_to_png`'s
    /// convergence loop. Ignored by default: needs a GPU, the large
    /// fixture, and writes a file. Point `MESH_SNAP_OUT` at an absolute
    /// path to control the output location.
    #[test]
    #[ignore]
    fn imported_azalea_renders_faithfully_to_png() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        // MESH_SNAP_GLB overrides the fixture — the P2 held-out-input gate
        // (SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md §5 P2) points this at a
        // >1-object model NOT the azalea the transform-port swap was
        // developed against, to prove the group-placement wiring generalizes.
        let path = std::env::var("MESH_SNAP_GLB")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| azalea_fixture_path());
        if !path.exists() {
            eprintln!(
                "imported_azalea_renders_faithfully_to_png: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble import graph");
        println!("imported model report: {report:?}");

        let (w, h) = (768u32, 768u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();

        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("assembled azalea graph must build through PresetRuntime::from_def_with_device");

        let target = RenderTarget::new(&device, w, h, format, "imported-azalea");
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        // BUG-100 (see docs/BUG_BACKLOG.md): `fraction > 0.02` alone is NOT
        // a texture-decode-completion signal — the material's own `ambient`
        // floor (`node.pbr_material`'s 0.18) lights the WHOLE silhouette
        // from frame 1, regardless of whether `node.gltf_texture_source`'s
        // background-thread decode has landed yet, so the old check broke
        // out of this loop on the very first or second attempt for models
        // whose base-color texture takes longer to decode than azalea's —
        // capturing (and, before this fix, permanently asserting on) an
        // under-textured, falsely-"near-black"-looking frame. Confirmed by
        // rendering: forcing extra attempts on the exact same unmodified
        // graph turns a near-black `cc0__japanese_apricot_prunus_mume.glb`/
        // `lowe.glb` capture into a fully lit, richly textured one — the
        // geometry/lighting/material rig was never the problem.
        //
        // Real completion signal: once the texture (and any other async
        // parse) has actually landed, the render is a pure function of a
        // static camera + static geometry + static light, so consecutive
        // frames stop changing. Poll until the readback is byte-identical
        // across `STABLE_STREAK` consecutive frames (not just non-black),
        // so a still-loading frame's mid-decode churn can't look "done".
        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut fraction = 0.0f64;
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        for attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("imported-azalea-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }

            let bytes_per_row = w * 8;
            let total_bytes = u64::from(h * bytes_per_row);
            let readback_buf = device.create_buffer_shared(total_bytes);
            let mut readback_enc = device.create_encoder("imported-azalea-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();

            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            let total = (w * h) as usize;
            fraction = non_black as f64 / total as f64;

            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());

            if stable_count >= STABLE_STREAK {
                println!(
                    "imported_azalea_renders_faithfully_to_png: converged on attempt {attempt} \
                     (non-black fraction {fraction:.4}, stable for {STABLE_STREAK} frames)"
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        let total = (w * h) as usize;
        println!(
            "imported_azalea_renders_faithfully_to_png: non-black pixel fraction = {fraction:.4} \
             ({max_attempts} attempts budget, total {total})"
        );
        assert!(
            fraction > 0.02,
            "expected >2% non-black pixels after polling for both background parses, got \
             {fraction:.4} — likely a broken importer graph, a parse that never landed, or empty geometry"
        );

        let out_path = std::env::var("MESH_SNAP_OUT")
            .unwrap_or_else(|_| "target/mesh-snap/imported_azalea.png".to_string());
        if let Some(parent) = std::path::Path::new(&out_path).parent() {
            std::fs::create_dir_all(parent).expect("create output dir");
        }
        image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
        println!(
            "imported_azalea_renders_faithfully_to_png: wrote {out_path} (fraction {fraction:.4}, report {report:?})"
        );
    }

    #[cfg(feature = "gpu-proofs")]
    /// Stage-4 production-path proof: render the assembled azalea graph the way
    /// a real dropped-in generator LAYER renders it — through
    /// [`GeneratorRegistry::create_with_override`] with the import graph as the
    /// per-layer override and the imported preset id as `gen_type`. This is the
    /// exact per-layer call `GeneratorRenderer::render_all` makes for a layer
    /// carrying a `generator_graph`, and it differs from
    /// `imported_azalea_renders_faithfully_to_png` (which drives
    /// `PresetRuntime::from_def_with_device` directly) in one load-bearing way:
    /// `is_watched = false` routes through the **on-demand fusion** attempt
    /// (`fused_generator_def_for`) that the raw-def path skips. So this closes
    /// the last gap — proving the imported graph survives the fuser and renders
    /// through the same code an installed timeline layer hits.
    ///
    /// It also proves the registry-gate is a non-issue: the sanitized preset id
    /// (`cc0_oomurasaki_...`) is NOT in the bundled catalog, yet the override
    /// def renders anyway because `create_with_override` builds from the def.
    #[test]
    #[ignore]
    fn imported_azalea_renders_through_create_with_override_to_png() {
        use crate::generators::registry::GeneratorRegistry;
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let path = azalea_fixture_path();
        if !path.exists() {
            eprintln!(
                "imported_azalea_renders_through_create_with_override_to_png: fixture not found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble azalea");
        let preset_id = def
            .preset_metadata
            .as_ref()
            .expect("assembled def carries v2 metadata")
            .id
            .clone();
        println!(
            "create_with_override proof: preset id = {preset_id:?} (NOT in bundled catalog), report {report:?}"
        );

        let (w, h) = (768u32, 768u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = GeneratorRegistry::new(format);

        // is_watched = false -> production render path, INCLUDING the on-demand
        // fusion attempt the raw from_def proof never exercised.
        let mut generator = registry
            .create_with_override(device.arc(), &preset_id, Some(&def), w, h, false, None)
            .expect(
                "create_with_override must build the imported generator from its override def, \
                 even though the preset id is not in the bundled catalog",
            );

        let target = RenderTarget::new(&device, w, h, format, "imported-azalea-layer");
        let ctx = PresetContext {
            time: 0.0,
            beat: 0.0,
            dt: 1.0 / 60.0,
            width: w,
            height: h,
            output_width: w,
            output_height: h,
            aspect: 1.0,
            owner_key: 0,
            is_clip_level: false,
            frame_count: 0,
            anim_progress: 0.0,
            trigger_count: 0,
        };

        // BUG-100: same premature-convergence fix as
        // `imported_azalea_renders_faithfully_to_png` — `fraction > 0.02`
        // alone is satisfied by the material's ambient floor before the
        // texture decode lands, so require the readback to be stable
        // across `STABLE_STREAK` consecutive frames too.
        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut fraction = 0.0f64;
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        for attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("imported-azalea-layer-render");
                {
                    let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
                    generator.render(
                        &mut gpu,
                        &target.texture,
                        &ctx,
                        &manifold_core::params::ParamManifest::default(),
                    );
                }
                enc.commit_and_wait_completed();
            }

            let bytes_per_row = w * 8;
            let total_bytes = u64::from(h * bytes_per_row);
            let readback_buf = device.create_buffer_shared(total_bytes);
            let mut readback_enc = device.create_encoder("imported-azalea-layer-readback");
            readback_enc.copy_texture_to_buffer(&target.texture, &readback_buf, w, h, bytes_per_row);
            readback_enc.commit_and_wait_completed();

            let ptr = readback_buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };

            rgba = Vec::with_capacity((w * h * 4) as usize);
            let mut non_black = 0usize;
            for px in halves.chunks_exact(4) {
                let r = tonemap_channel(half_to_f32(px[0]));
                let g = tonemap_channel(half_to_f32(px[1]));
                let b = tonemap_channel(half_to_f32(px[2]));
                if r != 0 || g != 0 || b != 0 {
                    non_black += 1;
                }
                rgba.push(r);
                rgba.push(g);
                rgba.push(b);
                let a = half_to_f32(px[3]).clamp(0.0, 1.0);
                rgba.push((a * 255.0).round() as u8);
            }
            fraction = non_black as f64 / (w * h) as f64;

            if fraction > 0.02 && prev_rgba.as_deref() == Some(rgba.as_slice()) {
                stable_count += 1;
            } else {
                stable_count = 0;
            }
            prev_rgba = Some(rgba.clone());

            if stable_count >= STABLE_STREAK {
                println!(
                    "create_with_override proof: converged on attempt {attempt} (non-black {fraction:.4}, stable for {STABLE_STREAK} frames)"
                );
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }

        assert!(
            fraction > 0.02,
            "expected the imported layer to render non-black through create_with_override, got \
             {fraction:.4} — a broken fusion pass, a parse that never landed, or a registry-gate regression"
        );

        let out_path = std::env::var("MESH_SNAP_OUT")
            .unwrap_or_else(|_| "target/mesh-snap/imported_azalea_layer.png".to_string());
        if let Some(parent) = std::path::Path::new(&out_path).parent() {
            std::fs::create_dir_all(parent).expect("create output dir");
        }
        image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {out_path}: {e}"));
        println!("create_with_override proof: wrote {out_path} (fraction {fraction:.4})");
    }
}

