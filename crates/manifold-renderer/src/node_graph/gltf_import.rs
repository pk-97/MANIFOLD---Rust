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

use crate::node_graph::primitives::DEFAULT_NEAR as CAMERA_NEAR_DEFAULT;
use crate::node_graph::primitives::render_scene::OBJECT_SAFETY_MAX;

/// How many of the largest (by vertex count) objects get a per-object card
/// slider (currently just glass's Opacity knob). GLB_CONFORMANCE_DESIGN.md
/// D4: the graph wires EVERY material 1:1 — this cap is pure UI curation on
/// top of a fully-imported graph, never a reason to drop geometry. Distinct
/// from [`OBJECT_SAFETY_MAX`], which bounds the graph itself.
const CARD_CURATION_MAX: usize = 16;

/// Stable identity for the one outer-card text config every imported
/// preset carries: the source `.glb`/`.gltf` path.
const MODEL_FILE_PARAM_ID: &str = "model_file";
/// GLB_CONFORMANCE_DESIGN.md D6 — the HDRI environment's own Browse field,
/// a distinct string param from [`MODEL_FILE_PARAM_ID`] (the imported
/// .glb's path). Empty by default; `node.hdri_source` reads an empty path
/// as "nothing decoded" and clears its output to black.
const HDRI_FILE_PARAM_ID: &str = "hdri_file";

/// What the assembler did, for the caller (importer UI, tests) to report
/// or warn on. Not part of the graph itself.
#[derive(Debug, Clone)]
pub struct ImportReport {
    /// Distinct materials with geometry, as parsed. Import is 1:1
    /// (GLB_CONFORMANCE_DESIGN.md D4) — always equal to `object_count`.
    pub material_count: usize,
    /// Objects wired into `node.render_scene` — always equal to
    /// `material_count`; nothing is ever dropped for exceeding a count
    /// (`assemble_import_graph` errors instead, see [`OBJECT_SAFETY_MAX`]).
    pub object_count: usize,
    /// How many objects got a `node.gltf_texture_source` → `base_color_map_N` wire.
    pub textures_wired: usize,
    /// Triangle-list vertices belonging to glTF's unassigned default
    /// material — v1 does not import these (mirrors
    /// [`gltf_load::GltfImportSummary::default_material_vertex_count`]).
    pub default_material_vertex_count: u32,
    /// Always `true` today — the assembler always synthesizes a framing
    /// camera (the glb's own embedded cameras, if any, are not yet
    /// consumed). Kept as a field so a future embedded-camera path has
    /// somewhere to report `false`.
    pub camera_synthesized: bool,
    /// D9 doctrine ("every import produces a report") applied to the
    /// per-material features F-P4 parses but cannot yet map: clearcoat
    /// (Deferred #1), transmission (report-only until F-P5), and BLEND
    /// materials downgraded to Mask cutout (the F-P5 stopgap). One line per
    /// occurrence, naming the material. Never silently dropped.
    pub report_lines: Vec<String>,
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

/// Wire one glTF map texture (normal / metallic-roughness / occlusion /
/// emissive) into this object's group: creates a `node.gltf_texture_source`
/// (or reuses one already created for the same `texture_index` within this
/// object — D5's ORM-packing case, where the occlusion and
/// metallic-roughness maps are the same physical image), adds the group's
/// outward `port_name` interface port, wires the source into it, and adds
/// the outer-card Model File → source-node `path` string binding (the same
/// convention `assemble_import_graph`'s base-color wiring above uses).
/// `cache` is scoped to ONE object (`k`) — keyed by glTF `texture_index` so
/// a second map wired from the same physical image reuses the first map's
/// decode rather than doubling the GPU decode + memory cost.
#[allow(clippy::too_many_arguments)]
fn wire_map_texture(
    tex_index: u32,
    color_space: u32,
    node_prefix: &str,
    port_name: &str,
    k: usize,
    path_str: &str,
    fresh_id: &mut impl FnMut() -> u32,
    group_nodes: &mut Vec<EffectGraphNode>,
    group_wires: &mut Vec<EffectGraphWire>,
    outputs: &mut Vec<InterfacePortDef>,
    string_bindings: &mut Vec<StringBindingDef>,
    cache: &mut std::collections::HashMap<u32, (u32, String)>,
    out_id: u32,
) {
    let (node_numeric_id, _node_id_str) = if let Some(existing) = cache.get(&tex_index) {
        existing.clone()
    } else {
        let node_id_str = format!("{node_prefix}_{k}");
        let tid = fresh_id();
        let mut node = plain_node(tid, &node_id_str, "node.gltf_texture_source", &node_id_str);
        node.params.insert("texture_index".to_string(), int(tex_index as i32));
        node.params.insert("color_space".to_string(), enum_val(color_space));
        // Same v1 default the base-color wiring uses — see its TODO about
        // threading real per-texture dimensions through the summary.
        node.params.insert("width".to_string(), int(1024));
        node.params.insert("height".to_string(), int(1024));
        group_nodes.push(node);

        string_bindings.push(StringBindingDef {
            id: MODEL_FILE_PARAM_ID.to_string(),
            label: "Model File".to_string(),
            default_value: path_str.to_string(),
            target: BindingTarget::Node {
                node_id: NodeId::new(&node_id_str),
                param: "path".to_string(),
            },
        });

        let entry = (tid, node_id_str);
        cache.insert(tex_index, entry.clone());
        entry
    };

    outputs.push(InterfacePortDef { name: port_name.to_string(), port_type: "Texture2D".to_string() });
    group_wires.push(wire(node_numeric_id, "out", out_id, port_name));
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

/// Default softbox dome-fill radiance stamped on imported rigs (F-P7) —
/// node param and Fill Light card default stay in sync through this one
/// constant. Tuned against the DamagedHelmet/AMG probe renders: enough
/// broad radiance that metallic surfaces read their albedo, low enough
/// that the black-void product look survives.
const IMPORT_FILL_DEFAULT: f32 = 0.6;

/// Default softbox strip intensity stamped on imported rigs (F-P7): half
/// the primitive's own 6.0 default. With the fill dome supplying the broad
/// radiance, full-strength strips dominate every curved reflection (the
/// banded-visor look); 3.0 keeps the chrome-streak character as an accent.
/// The Strip Lights card fader dials it live.
const IMPORT_STRIPS_DEFAULT: f32 = 3.0;

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
/// faithfully: one `node.render_scene` object per distinct material — 1:1,
/// no truncation (GLB_CONFORMANCE_DESIGN.md D4) — each fed its
/// material-filtered geometry + base-color texture (if any) + a PBR
/// material, framed by a synthesized orbit camera sized to the glb's
/// bounding box, lit by one sun light, under a baked IBL envmap (required —
/// `node.pbr_material` is degenerate without one). Pure function: one CPU
/// parse via [`gltf_load::gltf_import_summary`], no GPU, no other I/O.
///
/// Errors when the glb has no materials with geometry (nothing to import),
/// or when it has more than [`OBJECT_SAFETY_MAX`] materials with geometry
/// (a real GPU/port-list safety bound, D4 — never silently truncated) —
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

    // GLB_CONFORMANCE_DESIGN.md D4: import is 1:1 — every material with
    // geometry gets its own render_scene object, never a truncated prefix.
    // `OBJECT_SAFETY_MAX` (1024) is a real GPU/port-list safety bound, not a
    // curation cap: an asset beyond it errors loudly instead of silently
    // dropping geometry (the AMG GT3's black body, BUG-163, was exactly
    // this — 14 of 78 materials, including the livery, dropped over the old
    // 64-object cap).
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
    // Largest-by-vertex-count first: not a truncation boundary anymore
    // (every material is wired), but it IS the ordering the card curation
    // below relies on — the largest CARD_CURATION_MAX objects by vertex
    // count get per-object sliders, everything after them still gets full
    // geometry/material/texture wiring, just no card exposure.
    let mut materials = summary.materials.clone();
    materials.sort_by(|a, b| b.vertex_count.cmp(&a.vertex_count));
    let n = materials.len();
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
    // BUG-165/BUG-169 root cause (diagnosed via GLB_XFAIL_BURNDOWN_DESIGN.md
    // P1's `--trace` instrument): `node.orbit_camera`'s `near` clip plane
    // defaults to a fixed 0.05 (camera_orbit.rs), which was never scaled to
    // the framed object's own size. `distance = 2.2 * radius` already
    // scales with the object, so the object's front face sits at
    // `distance - radius == 1.2 * radius` from the camera — for any object
    // with `radius` below ~0.042 (BoomBox: 0.0172, MetalRoughSpheresNoTextures:
    // 0.0056 — both real-world-scale Khronos assets authored in meters),
    // the fixed near plane sits IN FRONT of the object and the whole frame
    // clips to black every frame (confirmed via `--trace`: io_pending goes
    // false almost immediately and the frame stays byte-stable-black from
    // frame 0/1 — not a decode race, ruling out the BUG-165 (a) hypothesis;
    // BUG-169's "lighting/material" hypothesis was also wrong — same
    // mechanism, not a texture-less-material bug).
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
    nodes.push(envmap_node);

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
    cam_node.params.insert("fov_y".to_string(), float(0.9));
    cam_node.params.insert("look_y".to_string(), float(0.0));
    // BUG-165/BUG-169 fix — see `near_clip` computation above.
    cam_node.params.insert("near".to_string(), float(near_clip));
    nodes.push(cam_node);

    // Physical lens (CINEMATIC_POST D6): sits between the raw orbit camera
    // and render_scene/ao. No depth-of-field consumer wired anymore (the
    // "dof" group was removed 2026-07-15 for buggy visuals) and no
    // motion_blur consumer either (see the motion_blur removal note below),
    // so `shutter_angle`/`focus_distance`/`f_stop` are along for the ride
    // only insofar as `node.camera_lens` requires them — nothing downstream
    // reads them today.
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
    // D9 doctrine — see `ImportReport::report_lines`.
    let mut report_lines: Vec<String> = Vec::new();

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

        // D9 — every unmapped feature this material carries is a report
        // line, never a silent drop. GLB_CONFORMANCE_DESIGN.md G-P5/D5:
        // clearcoatFactor/clearcoatRoughnessFactor are now REAL mappings
        // (below, on `node.pbr_material`) — only a TEXTURED coat
        // (clearcoatTexture/clearcoatRoughnessTexture/
        // clearcoatNormalTexture, factor-only v1) stays report-only
        // (Deferred #2). Transmission and BLEND are also real mappings —
        // IMPORT_FIDELITY_DESIGN.md D8/F-P5 maps both to a real `Blend`
        // material below, so they produce no line here.
        if m.clearcoat_has_texture {
            report_lines.push(format!(
                "{group_name}: KHR_materials_clearcoat has a clearcoatTexture/clearcoatRoughnessTexture/clearcoatNormalTexture — only the factors (clearcoatFactor/clearcoatRoughnessFactor) are imported in v1, the texture(s) are not sampled (report-only)"
            ));
        }
        // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_texture_transform is
        // applied per-map (all five families) — the only variant still
        // unmapped is a texCoord index override (v1 imports TEXCOORD_0
        // only), which is reported rather than silently dropped.
        if m.uv_tex_coord_override {
            report_lines.push(format!(
                "{group_name}: KHR_texture_transform.texCoord override — only TEXCOORD_0 is imported in v1, the override is ignored (report-only; the transform itself IS applied)"
            ));
        }
        if m.specular_has_texture {
            report_lines.push(format!(
                "{group_name}: KHR_materials_specular has a specularTexture/specularColorTexture — only the factor (specularFactor/specularColorFactor) is imported in v1, the texture is not sampled (report-only)"
            ));
        }
        // `render_scene` shipped no per-object normal-scale / occlusion-strength
        // uniform (F-P2's texture ports carry no multiplier) — a non-neutral
        // value is genuinely unmapped, not silently dropped, so it's a report
        // line rather than an applied effect. Neutral (1.0, or no texture
        // wired) produces no line — the common case stays quiet.
        if m.normal_texture.is_some() && (m.normal_scale - 1.0).abs() > 1e-4 {
            report_lines.push(format!(
                "{group_name}: normalTexture.scale = {:.2} (≠1.0) not applied — render_scene has no per-object normal-scale port yet (report-only)",
                m.normal_scale
            ));
        }
        if m.occlusion_texture.is_some() && (m.occlusion_strength - 1.0).abs() > 1e-4 {
            report_lines.push(format!(
                "{group_name}: occlusionTexture.strength = {:.2} (≠1.0) not applied — render_scene has no per-object occlusion-strength port yet (report-only)",
                m.occlusion_strength
            ));
        }
        // IMPORT_FIDELITY_DESIGN.md D8: glTF BLEND and
        // KHR_materials_transmission both become a real `Blend` material.
        // One formula covers both — `transmission_factor == 0.0` (plain
        // BLEND, no transmission extension) reduces to the material's own
        // base_color.a unchanged.
        let is_glass = m.was_blend || m.transmission_factor > 0.0;
        let effective_alpha =
            (m.base_color_factor[3] * (1.0 - m.transmission_factor)).clamp(0.0, 1.0);

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
        // IMPORT_FIDELITY_DESIGN.md D8: `effective_alpha` folds the
        // transmission formula in; for a plain opaque/mask material
        // (transmission_factor == 0.0) this is exactly base_color.a.
        mat_node
            .params
            .insert("color_a".to_string(), float(effective_alpha));
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
        // `emission_intensity` is the existing wired multiplier on
        // `node.pbr_material` — `KHR_materials_emissive_strength` folds
        // into it directly rather than growing a new param (D5: the
        // strength extension IS a multiplier on the same quantity this
        // param already controls). No extension present → factor 1.0, so
        // an emissive material still needs SOME emissive factor to glow
        // (matches the pre-F-P4 "any factor channel > 0" gate).
        let emissive_lit = m.emissive.iter().any(|&c| c > 0.0);
        mat_node.params.insert(
            "emission_intensity".to_string(),
            float(if emissive_lit { m.emissive_strength } else { 0.0 }),
        );
        mat_node.params.insert(
            "alpha_mode".to_string(),
            enum_val(if is_glass {
                2 // Blend
            } else if m.alpha_mask {
                1 // Mask
            } else {
                0 // Opaque
            }),
        );
        mat_node
            .params
            .insert("alpha_cutoff".to_string(), float(m.alpha_cutoff));
        // GLB_CONFORMANCE_DESIGN.md G-P4/D5: KHR_materials_specular + ior
        // → F0 scale (`fs_pbr`); KHR_texture_transform → base-color UV
        // affine (`resolve_albedo`). Every field defaults to the neutral
        // value verified in `gltf_load.rs` (ior=1.5, specular_factor=1.0,
        // specular_color_factor=[1,1,1], identity uv transform), so a
        // material without these extensions wires byte-identical params.
        mat_node.params.insert("ior".to_string(), float(m.ior));
        mat_node
            .params
            .insert("specular".to_string(), float(m.specular_factor));
        mat_node
            .params
            .insert("specular_tint_r".to_string(), float(m.specular_color_factor[0]));
        mat_node
            .params
            .insert("specular_tint_g".to_string(), float(m.specular_color_factor[1]));
        mat_node
            .params
            .insert("specular_tint_b".to_string(), float(m.specular_color_factor[2]));
        // GLB_CONFORMANCE_DESIGN.md G-P5/D5: KHR_materials_clearcoat
        // factors → the second GGX lobe (`fs_pbr`). Defaults (0.0/0.0)
        // reproduce byte-identical pre-G-P5 output — see `gltf_load.rs`.
        mat_node
            .params
            .insert("clearcoat".to_string(), float(m.clearcoat_factor));
        mat_node.params.insert(
            "clearcoat_roughness".to_string(),
            float(m.clearcoat_roughness_factor),
        );
        // Per-map KHR_texture_transform affines (G-P4) — one 6-param set
        // per map family, identity when the extension is absent.
        let parts = ["m00", "m01", "m10", "m11", "tx", "ty"];
        for (prefix, xf) in [
            ("uv_", &m.base_color_uv_transform),
            ("nrm_uv_", &m.normal_uv_transform),
            ("mr_uv_", &m.mr_uv_transform),
            ("occ_uv_", &m.occlusion_uv_transform),
            ("em_uv_", &m.emissive_uv_transform),
        ] {
            for (part, value) in parts.iter().zip(xf.iter()) {
                mat_node
                    .params
                    .insert(format!("{prefix}{part}"), float(*value));
            }
        }
        group_nodes.push(mat_node);

        // No per-object Metallic/Roughness card sliders (Peter, 2026-07-15:
        // "no need to modify them and they explode the card" — with one pair
        // per object, a multi-object import's card grew unusably long). The
        // material node above still carries the glTF's own metallic/
        // roughness values; only the card exposure is gone.
        // IMPORT_FIDELITY_DESIGN.md D8/F-P5 performer gesture: glass objects
        // ONLY get an Opacity knob (solid → ghost mid-set on the material's
        // alpha) — opaque/mask objects don't expose it, matching the
        // "inline mux option table params" discipline of not over-exposing
        // every knob to every object. GLB_CONFORMANCE_DESIGN.md D4: card
        // curation is UI-only and caps at CARD_CURATION_MAX — `materials`
        // is sorted largest-vertex-count-first above, so `k < CARD_CURATION_MAX`
        // IS "the largest 16"; every object still gets full graph wiring
        // regardless of `k`, this gate only withholds the card slider.
        if is_glass && k < CARD_CURATION_MAX {
            let opacity_id = format!("opacity_{k}");
            card_params.push(card_param(&opacity_id, "Opacity", 0.0, 1.0, effective_alpha, false, &group_name));
            card_bindings.push(card_binding(
                &opacity_id, "Opacity", effective_alpha, &mat_node_id, "color_a", 1.0,
            ));
        }
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

        // D3/D5/D6 — normal / metallic-roughness / occlusion / emissive
        // maps. `map_tex_cache` is scoped to THIS object and keyed by glTF
        // `texture_index`: ORM-packed files (occlusion index == mr index,
        // a common glTF convention) reuse the same `node.gltf_texture_source`
        // for both ports instead of decoding the same physical image
        // twice. Colour space per D6: normal/MR/occlusion decode linear
        // (data maps — the raw bytes ARE the value), emissive decodes sRGB
        // (a colour map, same as base-colour).
        let mut map_tex_cache: std::collections::HashMap<u32, (u32, String)> =
            std::collections::HashMap::new();
        let has_normal = m.normal_texture.is_some();
        if let Some(tex_index) = m.normal_texture {
            wire_map_texture(
                tex_index,
                1, // Linear
                "normal_tex",
                "normalMap",
                k,
                &path_str,
                &mut fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut outputs,
                &mut string_bindings,
                &mut map_tex_cache,
                out_id,
            );
        }
        let has_mr = m.mr_texture.is_some();
        if let Some(tex_index) = m.mr_texture {
            wire_map_texture(
                tex_index,
                1, // Linear
                "mr_tex",
                "mrMap",
                k,
                &path_str,
                &mut fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut outputs,
                &mut string_bindings,
                &mut map_tex_cache,
                out_id,
            );
        }
        let has_occlusion = m.occlusion_texture.is_some();
        if let Some(tex_index) = m.occlusion_texture {
            wire_map_texture(
                tex_index,
                1, // Linear
                "occlusion_tex",
                "occlusionMap",
                k,
                &path_str,
                &mut fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut outputs,
                &mut string_bindings,
                &mut map_tex_cache,
                out_id,
            );
        }
        let has_emissive = m.emissive_texture.is_some();
        if let Some(tex_index) = m.emissive_texture {
            wire_map_texture(
                tex_index,
                0, // sRGB — a colour map, same convention as base-colour
                "emissive_tex",
                "emissiveMap",
                k,
                &path_str,
                &mut fresh_id,
                &mut group_nodes,
                &mut group_wires,
                &mut outputs,
                &mut string_bindings,
                &mut map_tex_cache,
                out_id,
            );
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
        if has_normal {
            wires.push(wire(group_id, "normalMap", render_id, &format!("normal_map_{k}")));
        }
        if has_mr {
            wires.push(wire(group_id, "mrMap", render_id, &format!("mr_map_{k}")));
        }
        if has_occlusion {
            wires.push(wire(group_id, "occlusionMap", render_id, &format!("occlusion_map_{k}")));
        }
        if has_emissive {
            wires.push(wire(group_id, "emissiveMap", render_id, &format!("emissive_map_{k}")));
        }
    }

    nodes.push(render_node);

    // No atmosphere node (fog + god rays removed, Peter 2026-07-15): the
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

    // Depth of Field group removed (Peter, 2026-07-15): the coc_dilate/
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

    // Shared framing / light / environment card knobs. These come LAST in
    // `card_params` (after the per-object material knobs) but read first on
    // the card as the primary performance controls. Angle sliders store
    // RADIANS and carry `is_angle` (the app-wide convention — the slider
    // shows/edits degrees, storage is radians), so the binding is a
    // pass-through (`scale = 1.0`); mixing a degrees store with an `is_angle`
    // formatter double-converts (40° → 2298°). Defaults mirror the
    // `camera`/`sun`/`envmap` node params set above so the card is a faithful
    // mirror of the assembled look.
    // Both angle sliders `wraps: true` (Peter, 2026-07-15: "properly wrap 360
    // degrees") — a drag/modulation that crosses ±180° loops back round
    // instead of sticking at the edge (`constrain_to_range`'s wrap arm,
    // `manifold-core/src/params.rs`). Tilt widens from its old ±89° clamp to
    // the same full ±180° span as orbit: `camera_orbit`'s
    // `distance*sin(tilt)`/`cos(tilt)` spherical math is continuous through
    // the poles, so a full loop is a smooth over-the-top orbit, not a
    // singularity.
    let mut cam_orbit_param = card_param(
        "cam_orbit", "Camera Orbit", -180.0 * DEG2RAD, 180.0 * DEG2RAD, 0.7, true, "Camera",
    );
    cam_orbit_param.wraps = true;
    card_params.push(cam_orbit_param);
    card_bindings.push(card_binding(
        "cam_orbit", "Camera Orbit", 0.7, "camera", "orbit", 1.0,
    ));
    let mut cam_tilt_param = card_param(
        "cam_tilt", "Camera Tilt", -180.0 * DEG2RAD, 180.0 * DEG2RAD, 0.3, true, "Camera",
    );
    cam_tilt_param.wraps = true;
    card_params.push(cam_tilt_param);
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
    // D7 sun coherence (2026-07-15, Peter: "place these fake strips and
    // lights in the same positions as the real scene lights so it looks
    // coherent") — each sun_x/y/z macro fans out to TWO targets: the sun
    // `node.light`'s position (unchanged, pre-existing) AND the envmap's
    // new `sun_x/sun_y/sun_z` disc-direction params (F-P2/F-P3). Direction
    // params bind 1:1 (no conversion math) — `node.bake_environment`
    // normalizes the raw vector internally, and `aim` stays fixed at the
    // origin, so this object's `pos_*` IS already the sun's direction. One
    // fader now moves illumination, shadow, AND envmap reflection together.
    card_params.push(card_param("sun_x", "Sun X", -15.0, 15.0, 5.0, false, "Sun"));
    card_bindings.push(card_binding("sun_x", "Sun X", 5.0, "sun", "pos_x", 1.0));
    card_bindings.push(card_binding("sun_x", "Sun X", 5.0, "envmap", "sun_x", 1.0));
    card_params.push(card_param("sun_y", "Sun Y", -15.0, 15.0, 2.0, false, "Sun"));
    card_bindings.push(card_binding("sun_y", "Sun Y", 2.0, "sun", "pos_y", 1.0));
    card_bindings.push(card_binding("sun_y", "Sun Y", 2.0, "envmap", "sun_y", 1.0));
    card_params.push(card_param("sun_z", "Sun Z", -15.0, 15.0, 3.0, false, "Sun"));
    card_bindings.push(card_binding("sun_z", "Sun Z", 3.0, "sun", "pos_z", 1.0));
    card_bindings.push(card_binding("sun_z", "Sun Z", 3.0, "envmap", "sun_z", 1.0));
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
    // band and leaves the bright zenith/sun terms lighting the scene. D7
    // (F-P4): default flips to 1.0 — the softbox black-void studio is now
    // the import default (envmap node param above), so the card's default
    // must mirror it (range stays 0–4, unchanged).
    card_params.push(card_param("env_intensity", "Environment", 0.0, 4.0, 1.0, false, "Environment"));
    card_bindings.push(card_binding(
        "env_intensity", "Environment", 1.0, "envmap", "intensity", 1.0,
    ));
    // G-P6: the Environment master also drives the HDRI branch's exposure
    // gain (see the hdri_gain node above), so the fader is live in BOTH
    // env_mode positions — softbox scales inside the bake, HDRI scales the
    // decoded map. Each mode passes through exactly one of the two targets,
    // so there is no double-scaling. Same fan-out pattern as sun_x/y/z.
    card_bindings.push(card_binding(
        "env_intensity", "Environment", 1.0, "hdri_gain", "gain", 1.0,
    ));
    // F-P7 — the softbox dome fill (see the envmap node above). Separate
    // from `env_intensity` (which scales strips + disc + fill together):
    // this one moves ONLY the broad dome radiance, i.e. how much "world"
    // the metals reflect, without touching the strip highlights.
    card_params.push(card_param(
        "env_fill", "Fill Light", 0.0, 2.0, IMPORT_FILL_DEFAULT, false, "Environment",
    ));
    card_bindings.push(card_binding(
        "env_fill", "Fill Light", IMPORT_FILL_DEFAULT, "envmap", "fill", 1.0,
    ));
    // The strip emitters' own fader (F-P7): strips are the chrome-streak
    // accent, the fill is the world — independent faders, deliberately not
    // folded into `env_intensity` (which scales the whole bake).
    card_params.push(card_param(
        "env_strips", "Strip Lights", 0.0, 12.0, IMPORT_STRIPS_DEFAULT, false, "Environment",
    ));
    card_bindings.push(card_binding(
        "env_strips", "Strip Lights", IMPORT_STRIPS_DEFAULT, "envmap", "emitter_intensity", 1.0,
    ));
    // GLB_CONFORMANCE_DESIGN.md D6 — HDRI environment mode. env_mode picks
    // between the softbox bake (default, index 0) and the decoded HDRI file
    // (index 1) via `env_select`'s `selector` param (a plain Float — the
    // node.switch_texture family, not an Enum-typed node param — so the
    // binding is a Float pass-through like the mux-option-table pattern,
    // not EnumRound). `env_mode = 1` with an empty `hdri_file` reads as
    // black (node.hdri_source clears `out` to black until a file decodes),
    // same "nothing wired yet" convention as every other unwired texture
    // source in this graph.
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
    // performer picks one; `node.hdri_source` reads that as "nothing
    // decoded yet" and clears `out` to black (step 6 of its `run()`),
    // which env_mode=0 (Softbox, the default) never reaches anyway.
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
    // node (Peter 2026-07-15) — see the removal comment in
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

    /// Build a minimal, valid `.glb` with `n` distinct materials, each owning
    /// exactly one triangle (so every material has geometry and therefore
    /// counts toward `ImportReport::material_count`) — hand-rolled binary
    /// container (12-byte header + JSON chunk + BIN chunk, no external
    /// `.bin`/textures, no `uri` on the buffer so it resolves to the BIN
    /// chunk per spec §Binary glTF). GLB_CONFORMANCE_DESIGN.md G-P2: proves
    /// the FULL production parse path (`gltf::import` → `gltf_import_summary`
    /// → `build_import_graph`) imports every material 1:1, not just the
    /// graph-assembly half a synthetic [`GltfImportSummary`] would exercise.
    /// Written to the OS temp dir, not committed — a builder fn, not a
    /// binary asset (the phase brief's explicit call).
    fn write_synthetic_multimaterial_glb(n: usize) -> std::path::PathBuf {
        let mut accessors = Vec::with_capacity(n);
        let mut buffer_views = Vec::with_capacity(n);
        let mut materials = Vec::with_capacity(n);
        let mut primitives = Vec::with_capacity(n);
        let mut bin = Vec::with_capacity(n * 36);

        for i in 0..n {
            // One triangle per material, spread along X so no two overlap —
            // cosmetic, but keeps bbox/normal math non-degenerate.
            let ox = i as f32 * 2.0;
            let tri: [[f32; 3]; 3] = [[ox, 0.0, 0.0], [ox + 1.0, 0.0, 0.0], [ox, 1.0, 0.0]];
            for v in &tri {
                for c in v {
                    bin.extend_from_slice(&c.to_le_bytes());
                }
            }
            let byte_offset = i * 36;
            buffer_views.push(serde_json::json!({
                "buffer": 0,
                "byteOffset": byte_offset,
                "byteLength": 36,
            }));
            accessors.push(serde_json::json!({
                "bufferView": i,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [ox, 0.0, 0.0],
                "max": [ox + 1.0, 1.0, 0.0],
            }));
            materials.push(serde_json::json!({
                "name": format!("Mat{i}"),
                "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] },
            }));
            // Mode omitted — glTF's default primitive mode is 4 (TRIANGLES).
            primitives.push(serde_json::json!({
                "attributes": { "POSITION": i },
                "material": i,
            }));
        }

        let doc = serde_json::json!({
            "asset": { "version": "2.0" },
            "scene": 0,
            "scenes": [{ "nodes": [0] }],
            "nodes": [{ "mesh": 0 }],
            "meshes": [{ "primitives": primitives }],
            "accessors": accessors,
            "bufferViews": buffer_views,
            "materials": materials,
            "buffers": [{ "byteLength": bin.len() }],
        });
        let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");

        // GLB container: header + JSON chunk (space-padded to 4 bytes) + BIN
        // chunk (zero-padded to 4 bytes). Chunk type magics per the Binary
        // glTF spec: 0x4E4F534A = "JSON", 0x004E4942 = "BIN\0".
        let mut json_padded = json_bytes;
        while json_padded.len() % 4 != 0 {
            json_padded.push(b' ');
        }
        let mut bin_padded = bin;
        while bin_padded.len() % 4 != 0 {
            bin_padded.push(0);
        }
        let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

        let mut glb = Vec::with_capacity(total_len);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total_len as u32).to_le_bytes());
        glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_padded);
        glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin_padded);

        let path = std::env::temp_dir().join(format!(
            "manifold_synthetic_{n}mat_{}_{}.glb",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
        path
    }

    /// GLB_CONFORMANCE_DESIGN.md D4 / G-P2's named gate test: a 100-material
    /// synthetic asset — well past the old (dead) 64-object cap, well under
    /// `OBJECT_SAFETY_MAX` (1024) — imports every single material 1:1, no
    /// truncation. Also proves the card-curation split: only the largest
    /// `CARD_CURATION_MAX` (16) objects would get a per-object slider (none
    /// of these materials are glass, so no Opacity sliders exist either way
    /// — the object/wire count is what this test pins).
    #[test]
    fn over_cap_asset_imports_one_to_one() {
        let path = write_synthetic_multimaterial_glb(100);
        let (def, report) = assemble_import_graph(&path).expect("assemble 100-material synthetic glb");
        std::fs::remove_file(&path).ok();

        assert_eq!(report.material_count, 100, "all 100 materials have geometry");
        assert_eq!(
            report.object_count, 100,
            "import is 1:1 — object_count must equal material_count, no truncation (D4)"
        );

        // Every material got its own render_scene wire — mesh_0..mesh_99 all
        // present, nothing dropped past the old 64-object boundary.
        for k in 0..100 {
            assert!(
                def.wires.iter().any(|w| w.to_port == format!("mesh_{k}")),
                "material {k} (past the old 64-object cap) must still wire mesh_{k}"
            );
            assert!(
                def.wires.iter().any(|w| w.to_port == format!("material_{k}")),
                "material {k} must still wire material_{k}"
            );
        }
        // render_scene's own `objects` param must reflect the true count,
        // not a clamped one.
        let render_node = def
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .expect("assembled graph has a render_scene node");
        assert_eq!(
            render_node.params.get("objects"),
            Some(&int(100)),
            "render_scene.objects must be the true unclamped count"
        );

        // Structural gate: the assembled graph — 100 objects, well past the
        // dead 64-object UI cap — must still compile through the real
        // registry (catches a bad port/wire before any GPU proof).
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(def, &registry, None)
            .expect("100-object import graph must compile through PresetRuntime::from_def");
    }

    /// D4's other half: a glb whose material count exceeds `OBJECT_SAFETY_MAX`
    /// (1024) must error loudly at import time — never silently truncate.
    /// `object_cap_exceeded_glb_errors_loudly_never_truncates` deliberately
    /// builds only ONE material past the limit (1025) rather than a much
    /// larger number — the synthetic-glb builder is O(n) JSON, and this test
    /// only needs to cross the boundary, not stress it.
    #[test]
    fn object_cap_exceeded_glb_errors_loudly_never_truncates() {
        let n = OBJECT_SAFETY_MAX as usize + 1;
        let path = write_synthetic_multimaterial_glb(n);
        let result = assemble_import_graph(&path);
        std::fs::remove_file(&path).ok();

        let err = result.expect_err("a glb past OBJECT_SAFETY_MAX must error, not truncate");
        assert!(
            err.contains(&n.to_string()) && err.contains(&OBJECT_SAFETY_MAX.to_string()),
            "error must name both the actual count and the safety bound, got: {err}"
        );
    }

    /// GLB_CONFORMANCE_DESIGN.md D4's card-curation half, plus the standard
    /// §5 round-trip rule (imports serialize — same pattern as
    /// `round_trip_preserves_map_wires_and_sun_coherence_bindings`): 20
    /// glass objects, largest-vertex-count-first — the first `CARD_CURATION_MAX`
    /// (16) get an Opacity card slider, the remaining 4 don't, but ALL 20
    /// still get full graph wiring. Every bit of that — object count, the
    /// curated/uncurated split, and the wiring — must survive a save/reload
    /// (JSON round trip of `EffectGraphDef`, the actual persisted artifact
    /// for an imported generator layer).
    #[test]
    fn card_curation_caps_at_16_but_wiring_and_round_trip_stay_1_to_1() {
        let n = 20;
        let materials: Vec<_> = (0..n)
            .map(|k| {
                let mut m = full_material(k as u32, &format!("Glass{k}"), (n - k) as u32 * 100);
                // All glass, so every object is a curation candidate — makes
                // the 16/4 split unambiguous.
                m.was_blend = true;
                m.transmission_factor = 0.5;
                m
            })
            .collect();
        let summary = GltfImportSummary {
            materials,
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_curation_round_trip.glb");
        let (def, report) = build_import_graph(&summary, path).expect("build 20-object graph");
        assert_eq!(report.object_count, 20, "1:1 — every glass object gets full wiring");

        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        for (def, label) in [(&def, "pre-reload"), (&reloaded, "post-reload")] {
            let meta = def.preset_metadata.as_ref().unwrap_or_else(|| panic!("{label}: v2 metadata"));
            let opacity_count = meta.params.iter().filter(|p| p.name == "Opacity").count();
            assert_eq!(
                opacity_count, CARD_CURATION_MAX,
                "{label}: exactly the largest {CARD_CURATION_MAX} objects get an Opacity slider"
            );
            for k in 0..CARD_CURATION_MAX {
                assert!(
                    meta.params.iter().any(|p| p.id == format!("opacity_{k}")),
                    "{label}: object {k} (top {CARD_CURATION_MAX}) must have a card slider"
                );
            }
            for k in CARD_CURATION_MAX..n {
                assert!(
                    !meta.params.iter().any(|p| p.id == format!("opacity_{k}")),
                    "{label}: object {k} (past the curation cap) must NOT have a card slider"
                );
            }

            // Curation is UI-only — full graph wiring survives for every
            // object, curated or not (the forbidden-move: "per-object slider
            // explosion" never becomes "per-object geometry drop").
            let flat = manifold_core::flatten::flatten_groups(def)
                .unwrap_or_else(|e| panic!("{label}: flatten failed: {e}"));
            let render = flat.nodes.iter().find(|n| n.type_id == "node.render_scene").unwrap();
            for k in 0..n {
                assert!(
                    flat.wires.iter().any(|w| w.to_node == render.id && w.to_port == format!("mesh_{k}")),
                    "{label}: object {k} must still wire mesh_{k} regardless of card curation"
                );
            }
        }

        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(reloaded, &registry, None)
            .expect("reloaded 20-object import graph must build through PresetRuntime::from_def");
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
        // GLB_CONFORMANCE_DESIGN.md D6: a second string param, `hdri_file`,
        // holds the HDRI environment's own Browse field — a separate file
        // from `model_file` (the imported .glb itself).
        assert_eq!(meta.string_params.len(), 2, "model_file + hdri_file string params");
        assert_eq!(meta.string_params[0].id, "model_file");
        assert!(meta.string_params[0].is_file_picker);
        assert_eq!(meta.string_params[1].id, "hdri_file");
        assert!(meta.string_params[1].is_file_picker);

        assert_eq!(
            meta.string_bindings.len(),
            5,
            "2 mesh + 2 texture path bindings (model_file) + 1 HDRI path binding (hdri_file)"
        );
        for b in &meta.string_bindings {
            assert!(b.id == "model_file" || b.id == "hdri_file", "unexpected string binding id {}", b.id);
            match &b.target {
                BindingTarget::Node { param, .. } => assert_eq!(param, "path"),
                other => panic!("expected a Node binding target, got {other:?}"),
            }
        }
        assert_eq!(
            meta.string_bindings.iter().filter(|b| b.id == "hdri_file").count(),
            1,
            "exactly one hdri_file binding, targeting the hdri node"
        );

        // Curated performance surface. Azalea has 2 objects → 4 camera + 5 sun
        // + 1 Environment + 1 Environment Mode (D6) + 1 Fill Light + 1 Strip
        // Lights (F-P7) + 1 Ambient = 14 framing/material sliders. No
        // Atmosphere section (fog + god rays removed with the atmosphere
        // node, Peter 2026-07-15), no Motion Blur (BUG-136), no per-object
        // Metallic/Roughness and no SSAO/DoF card sliders (Peter,
        // 2026-07-15: DoF removed for buggy visuals, AO/metallic/roughness
        // hidden — defaults still apply, just not on the card).
        assert_eq!(meta.params.len(), 14, "14 framing/material sliders");
        // Every param routes one-to-one except: the shared Ambient, which
        // fans out to every material's ambient (2 for azalea); D7's sun
        // coherence, where each of sun_x/sun_y/sun_z fans out to TWO targets
        // (the sun light AND the envmap's disc direction) — 3 extra
        // bindings; and G-P6's Environment master, which fans out to the
        // softbox bake's intensity AND the HDRI branch's exposure gain — 1
        // extra. 14 + 1 (ambient) + 3 (sun coherence) + 1 (env fan-out) = 19.
        assert_eq!(
            meta.bindings.len(),
            19,
            "14 params, Ambient fanned to 2 materials, sun_x/y/z each fanned to 2 targets, env_intensity fanned to bake + hdri gain"
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
        // Spot-check the shared framing knobs.
        for id in ["cam_orbit", "cam_dist", "sun_int", "env_intensity", "scene_ambient"] {
            assert!(meta.params.iter().any(|p| p.id == id), "missing card param `{id}`");
        }
        // Shared framing/light/environment knobs carry the fixed section names.
        let cam_orbit = meta.params.iter().find(|p| p.id == "cam_orbit").unwrap();
        assert_eq!(cam_orbit.section.as_deref(), Some("Camera"));
        let sun_int = meta.params.iter().find(|p| p.id == "sun_int").unwrap();
        assert_eq!(sun_int.section.as_deref(), Some("Sun"));
        let env_intensity = meta.params.iter().find(|p| p.id == "env_intensity").unwrap();
        assert_eq!(env_intensity.section.as_deref(), Some("Environment"));
        // D7 (F-P4): the black-void softbox studio is now the import
        // default, so Environment starts at 1.0 (range unchanged); the
        // shared Ambient fill still starts at 0 — softbox lighting comes
        // from the envmap + sun, not a flat fill floor.
        assert_eq!(env_intensity.default_value, 1.0, "environment bakes at softbox intensity 1.0 by default (D7)");
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
        // Orbit and tilt both wrap a full 360° instead of clamping at their
        // edges (Peter, 2026-07-15).
        assert!(cam_orbit.wraps, "camera orbit must wrap 360°");
        let cam_tilt = meta.params.iter().find(|p| p.id == "cam_tilt").unwrap();
        assert!(cam_tilt.wraps, "camera tilt must wrap 360°");
        assert!(
            (cam_tilt.min - (-std::f32::consts::PI)).abs() < 1e-4
                && (cam_tilt.max - std::f32::consts::PI).abs() < 1e-4,
            "camera tilt spans the full ±180° range to match orbit's wrap"
        );

        // GTAO and the lens are wired into the spine. No motion blur
        // (BUG-136 + fusion cost, see the removal comment in
        // `build_import_graph`), no depth-of-field group (Peter, 2026-07-15:
        // buggy visuals), no atmosphere node (fog + god rays removed,
        // Peter 2026-07-15).
        for present in ["node.ssao_gtao", "node.bilateral_blur", "node.camera_lens"] {
            assert!(
                def.nodes.iter().any(|n| n.type_id == present)
                    || def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                        g.nodes.iter().any(|inner| inner.type_id == present)
                    }),
                "imported graph must carry `{present}`"
            );
        }
        // `node.motion_blur` was removed (BUG-136: no visible effect live,
        // despite correct wiring — see the removal comment above);
        // `node.variable_blur` was P4's superseded DoF blur stage; and the
        // DoF chain itself (`coc_from_depth`/`coc_dilate`/`bokeh_gather`)
        // was removed 2026-07-15 for buggy visuals. None is reintroduced.
        for absent in [
            "node.motion_blur",
            "node.variable_blur",
            "node.coc_from_depth",
            "node.coc_dilate",
            "node.bokeh_gather",
            "node.atmosphere",
        ] {
            assert!(
                !def.nodes.iter().any(|n| n.type_id == absent)
                    && !def.nodes.iter().filter_map(|n| n.group.as_ref()).any(|g| {
                        g.nodes.iter().any(|inner| inner.type_id == absent)
                    }),
                "`{absent}` should not be in the imported graph"
            );
        }
        // No SSAO/metallic/roughness/DoF card sliders — the underlying nodes
        // keep their defaults, they're just no longer exposed on the card.
        for gone_prefix in ["ssao_", "metal_", "rough_", "dof_"] {
            assert!(
                !meta.params.iter().any(|p| p.id.starts_with(gone_prefix)),
                "no card param should start with `{gone_prefix}`"
            );
        }
        for gone in [
            "lens_focus", "lens_fstop", "lens_shutter", "lens_ev", "dof_radius",
            "motion_blur_px", "mb_shutter", "ssao_bias", "fog_density", "god_rays",
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
            normal_texture: None,
            normal_scale: 1.0,
            mr_texture: None,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive_texture: None,
            emissive_strength: 1.0,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0, 1.0, 1.0],
            specular_has_texture: false,
            base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            uv_tex_coord_override: false,
            transmission_factor: 0.0,
            clearcoat_factor: 0.0,
            clearcoat_roughness_factor: 0.0,
            clearcoat_has_texture: false,
            was_blend: false,
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

        // Top level: two per-object group boxes PLUS the "ao" presentation
        // group (CINEMATIC_POST; "dof" removed 2026-07-15), no bare producer
        // nodes.
        let groups: Vec<_> = def.nodes.iter().filter(|n| n.type_id == GROUP_TYPE_ID).collect();
        assert_eq!(groups.len(), 3, "2 object groups + ao");
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
        // Only the per-object groups carry a tint (CINEMATIC_POST's ao
        // group is an untinted presentation box, not per-object identity).
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
        assert_eq!(snap_groups.len(), 3, "editor snapshot shows 2 object + ao group boxes");
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

    /// A synthetic [`GltfMaterialInfo`] carrying every texture kind F-P4
    /// wires, with independent test-controlled fields for the three
    /// report-only features (clearcoat/transmission/BLEND). Defaults mirror
    /// a "fully-mapped, nothing extra" material — callers override only
    /// what a specific test cares about (Rust has no field-update syntax
    /// across `..` for `pub(crate)` structs outside the defining module, so
    /// this is a plain builder-by-closure, not `..Default::default()`).
    fn full_material(material_index: u32, name: &str, verts: u32) -> super::gltf_load::GltfMaterialInfo {
        use super::gltf_load::GltfMaterialInfo;
        GltfMaterialInfo {
            material_index,
            name: Some(name.to_string()),
            base_color_factor: [0.8, 0.8, 0.8, 1.0],
            metallic: 1.0,
            roughness: 0.4,
            emissive: [1.0, 0.5, 0.2],
            alpha_mask: false,
            alpha_cutoff: 0.5,
            base_color_texture: Some(0),
            normal_texture: Some(1),
            normal_scale: 1.0,
            mr_texture: Some(2),
            occlusion_texture: Some(3),
            occlusion_strength: 1.0,
            emissive_texture: Some(4),
            emissive_strength: 2.5,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0, 1.0, 1.0],
            specular_has_texture: false,
            base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            uv_tex_coord_override: false,
            transmission_factor: 0.0,
            clearcoat_factor: 0.0,
            clearcoat_roughness_factor: 0.0,
            clearcoat_has_texture: false,
            was_blend: false,
            vertex_count: verts,
        }
    }

    /// D6 colour-space pinning + D3 port-wiring: a synthetic material
    /// carrying all five texture kinds (base-colour, normal, MR, occlusion,
    /// emissive) must wire all four NEW ports (`normal_map_0`, `mr_map_0`,
    /// `occlusion_map_0`, `emissive_map_0`) into `node.render_scene`, each
    /// fed by a `node.gltf_texture_source` whose `color_space` matches D6:
    /// base-colour and emissive decode sRGB (0), normal/MR/occlusion decode
    /// Linear (1) — the data-map convention (raw bytes ARE the value).
    #[test]
    fn imports_all_map_kinds_with_correct_color_spaces() {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "Helmet", 1000)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_all_maps.glb");
        let (def, report) = build_import_graph(&summary, path).expect("build graph");
        assert_eq!(report.textures_wired, 1, "base-colour wired");

        // Flatten so the group-internal texture-source nodes and the
        // top-level render_scene wires are both queryable in one flat
        // node/wire list (same recipe the grouping-equivalence test above
        // uses).
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

        let render = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.render_scene")
            .expect("render_scene node");
        for port in ["normal_map_0", "mr_map_0", "occlusion_map_0", "emissive_map_0"] {
            assert!(
                flat.wires.iter().any(|w| w.to_node == render.id && w.to_port == port),
                "expected a wire into render_scene port `{port}`"
            );
        }

        // Each new map's own source node carries the D6-correct color_space.
        let expect_color_space = |prefix: &str, expected: u32| {
            let node = flat
                .nodes
                .iter()
                .find(|n| n.node_id.starts_with(prefix) && n.type_id == "node.gltf_texture_source")
                .unwrap_or_else(|| panic!("expected a `{prefix}*` gltf_texture_source node"));
            let cs = node.params.get("color_space").expect("color_space param set");
            assert_eq!(
                *cs,
                enum_val(expected),
                "`{prefix}*` color_space must be {expected} ({})",
                if expected == 0 { "sRGB" } else { "Linear" }
            );
        };
        expect_color_space("tex_", 0); // base-colour: sRGB
        expect_color_space("normal_tex_", 1); // normal: Linear
        expect_color_space("mr_tex_", 1); // metallic-roughness: Linear
        expect_color_space("occlusion_tex_", 1); // occlusion: Linear
        expect_color_space("emissive_tex_", 0); // emissive: sRGB

        // KHR_materials_emissive_strength folds into the existing
        // emission_intensity param rather than growing a new one (D5).
        let mat = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.pbr_material")
            .expect("pbr_material node");
        assert_eq!(
            mat.params.get("emission_intensity"),
            Some(&float(2.5)),
            "emissive_strength (2.5) must land on emission_intensity"
        );

        // Fully-mapped, nothing report-worthy: no clearcoat/transmission/BLEND lines.
        assert!(
            report.report_lines.is_empty(),
            "a fully-mapped material with no clearcoat/transmission/BLEND should report nothing, got {:?}",
            report.report_lines
        );
    }

    /// D5 ORM-packing: when `occlusion_texture` and `mr_texture` share the
    /// same glTF texture index (the common "one packed ORM image" case),
    /// the importer must wire ONE `node.gltf_texture_source` into BOTH
    /// `occlusion_map_0` and `mr_map_0` — never decode the same physical
    /// image twice.
    #[test]
    fn orm_packed_occlusion_and_mr_share_one_texture_source_node() {
        let mut m = full_material(0, "ORM", 500);
        m.occlusion_texture = Some(7);
        m.mr_texture = Some(7); // same physical image as occlusion
        let summary = GltfImportSummary {
            materials: vec![m],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_orm.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");

        let orm_sources: Vec<_> = flat
            .nodes
            .iter()
            .filter(|n| {
                n.type_id == "node.gltf_texture_source"
                    && n.params.get("texture_index") == Some(&int(7))
            })
            .collect();
        assert_eq!(
            orm_sources.len(),
            1,
            "occlusion_texture == mr_texture must decode through exactly ONE source node, found {}",
            orm_sources.len()
        );
        let render = flat.nodes.iter().find(|n| n.type_id == "node.render_scene").unwrap();
        let source_id = orm_sources[0].id;
        for port in ["occlusion_map_0", "mr_map_0"] {
            assert!(
                flat.wires
                    .iter()
                    .any(|w| w.to_node == render.id && w.to_port == port && w.from_node == source_id),
                "expected `{port}` wired directly from the shared ORM source node"
            );
        }
    }

    /// D9 doctrine ("every import produces a report") applied to G-P5's one
    /// remaining not-yet-mapped clearcoat feature: a TEXTURED coat.
    /// Transmission and BLEND (F-P5/D8) are real mappings, not
    /// report-only; clearcoat's FACTOR is now a real mapping too (G-P5/D5).
    /// An over-featured synthetic material carrying a textured clearcoat
    /// together with transmission and a BLEND alphaMode must report only
    /// the clearcoat texture, and must build a real `Blend` material with
    /// the transmission-folded alpha AND the clearcoat factor wired onto
    /// `node.pbr_material`.
    #[test]
    fn over_featured_material_reports_only_clearcoat_texture_and_maps_transmission_to_blend() {
        let mut m = full_material(0, "Kitchen Sink", 300);
        m.clearcoat_factor = 1.0;
        m.clearcoat_roughness_factor = 0.1;
        m.clearcoat_has_texture = true;
        m.transmission_factor = 0.9;
        m.was_blend = true;
        m.alpha_mask = false; // a real glTF BLEND material never sets MASK too
        m.base_color_factor = [0.9, 0.95, 1.0, 1.0];
        let summary = GltfImportSummary {
            materials: vec![m],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_over_featured.glb");
        let (def, report) = build_import_graph(&summary, path).expect("build graph");
        println!("over-featured report: {:#?}", report.report_lines);
        assert_eq!(
            report.report_lines.len(),
            1,
            "only the clearcoat texture remains report-only (the factor is now mapped)"
        );
        assert!(
            report.report_lines.iter().any(|l| l.contains("clearcoat")),
            "missing a clearcoat report line: {:?}",
            report.report_lines
        );
        assert!(
            !report.report_lines.iter().any(|l| l.contains("transmission") || l.contains("BLEND")),
            "transmission/BLEND must no longer produce report lines: {:?}",
            report.report_lines
        );

        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");
        let mat = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.pbr_material")
            .expect("pbr_material node");
        assert_eq!(
            mat.params.get("alpha_mode"),
            Some(&enum_val(2)),
            "transmission/BLEND material must map to alpha_mode Blend (2)"
        );
        // base_color.a (1.0) * (1 - transmission_factor 0.9) = 0.1
        let color_a = mat.params.get("color_a").expect("color_a set");
        match color_a {
            SerializedParamValue::Float { value } => assert!(
                (value - 0.1).abs() < 1e-4,
                "color_a must fold the transmission formula, got {value}"
            ),
            other => panic!("expected Float color_a, got {other:?}"),
        }
        // GLB_CONFORMANCE_DESIGN.md G-P5/D5: the factor IS mapped, only the
        // texture is report-only.
        assert_eq!(mat.params.get("clearcoat"), Some(&float(1.0)));
        assert_eq!(mat.params.get("clearcoat_roughness"), Some(&float(0.1)));
    }

    /// D7 sun coherence: each of the Sun X/Y/Z card macros must carry TWO
    /// binding targets — the sun `node.light`'s position (unchanged,
    /// pre-existing) AND the envmap's new `sun_x`/`sun_y`/`sun_z` disc-
    /// direction params — so performing the sun macro moves illumination,
    /// shadow, AND the envmap's reflected sun disc together (Peter,
    /// 2026-07-15: "place these fake strips and lights in the same
    /// positions as the real scene lights").
    #[test]
    fn sun_macros_bind_both_the_light_and_the_envmap_disc_direction() {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "Object", 100)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_sun.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");
        let meta = def.preset_metadata.as_ref().expect("v2 metadata");

        for (macro_id, sun_param, env_param) in
            [("sun_x", "pos_x", "sun_x"), ("sun_y", "pos_y", "sun_y"), ("sun_z", "pos_z", "sun_z")]
        {
            let bindings: Vec<_> = meta.bindings.iter().filter(|b| b.id == macro_id).collect();
            assert_eq!(
                bindings.len(),
                2,
                "`{macro_id}` must carry exactly 2 binding targets (sun light + envmap disc), got {}",
                bindings.len()
            );
            let targets_sun = bindings.iter().any(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    node_id.as_str() == "sun" && param == sun_param
                }
                _ => false,
            });
            let targets_envmap = bindings.iter().any(|b| match &b.target {
                BindingTarget::Node { node_id, param } => {
                    node_id.as_str() == "envmap" && param == env_param
                }
                _ => false,
            });
            assert!(targets_sun, "`{macro_id}` must bind the sun light's `{sun_param}`");
            assert!(
                targets_envmap,
                "`{macro_id}` must ALSO bind the envmap's `{env_param}` disc-direction param"
            );
            // Both bindings must carry the same default (scale 1.0, "no
            // conversion math in a binding" per D7) so the card reproduces
            // the assembled look with no drift on first frame.
            let defaults: std::collections::HashSet<_> =
                bindings.iter().map(|b| b.default_value.to_bits()).collect();
            assert_eq!(defaults.len(), 1, "`{macro_id}`'s two bindings must share one default value");
        }

        // D7 import defaults: softbox @ 1.0, not the legacy gradient @ 0.
        let flat = manifold_core::flatten::flatten_groups(&def).expect("flatten");
        let envmap = flat.nodes.iter().find(|n| n.type_id == "node.bake_environment").unwrap();
        assert_eq!(envmap.params.get("mode"), Some(&enum_val(1)), "import default mode = Softbox");
        assert_eq!(envmap.params.get("intensity"), Some(&float(1.0)), "import default intensity = 1.0");
        let env_intensity_param = meta.params.iter().find(|p| p.id == "env_intensity").unwrap();
        assert_eq!(env_intensity_param.default_value, 1.0, "Environment card default = 1.0");
        assert_eq!(env_intensity_param.min, 0.0);
        assert_eq!(env_intensity_param.max, 4.0, "range stays 0-4 (D7: only the default flips)");

        // F-P7 import defaults: dome fill on, strips at half the primitive
        // default — node params and card faders stay in sync through the
        // shared constants.
        assert_eq!(
            envmap.params.get("fill"),
            Some(&float(IMPORT_FILL_DEFAULT)),
            "import default fill = IMPORT_FILL_DEFAULT"
        );
        assert_eq!(
            envmap.params.get("emitter_intensity"),
            Some(&float(IMPORT_STRIPS_DEFAULT)),
            "import default strips = IMPORT_STRIPS_DEFAULT"
        );
        let fill_param = meta.params.iter().find(|p| p.id == "env_fill").unwrap();
        assert_eq!(fill_param.default_value, IMPORT_FILL_DEFAULT);
        let strips_param = meta.params.iter().find(|p| p.id == "env_strips").unwrap();
        assert_eq!(strips_param.default_value, IMPORT_STRIPS_DEFAULT);
    }

    /// BUG-036 round-trip gate: the assembled import graph — including the
    /// new map-port wires and the D7 sun-coherence dual bindings — must
    /// survive a save/reload cycle. `EffectGraphDef` (this function's
    /// return type) IS the persisted artifact for a generator layer's
    /// override (`ImportModelLayerCommand` stores it verbatim), so a JSON
    /// round trip through its own `Serialize`/`Deserialize` impl is the
    /// real save/reload path, not a stand-in for it. Asserts the wires and
    /// bindings survive AND that the reloaded def still builds through the
    /// production loader (`PresetRuntime::from_def`) — proving the ports
    /// resolve after reload, not only right after assembly.
    #[test]
    fn round_trip_preserves_map_wires_and_sun_coherence_bindings() {
        let summary = GltfImportSummary {
            materials: vec![full_material(0, "Helmet", 1000)],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_round_trip.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");

        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        let flat = manifold_core::flatten::flatten_groups(&reloaded).expect("flatten reloaded def");
        let render = flat.nodes.iter().find(|n| n.type_id == "node.render_scene").unwrap();
        for port in ["normal_map_0", "mr_map_0", "occlusion_map_0", "emissive_map_0"] {
            assert!(
                flat.wires.iter().any(|w| w.to_node == render.id && w.to_port == port),
                "reloaded def must still wire `{port}` — maps must stay bound after reload"
            );
        }
        let meta = reloaded.preset_metadata.as_ref().expect("reloaded v2 metadata");
        for macro_id in ["sun_x", "sun_y", "sun_z"] {
            let count = meta.bindings.iter().filter(|b| b.id == macro_id).count();
            assert_eq!(count, 2, "`{macro_id}`'s dual binding must survive reload");
        }

        // Modulation live after reload, not just structurally present: the
        // reloaded def must still build through the production loader.
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(reloaded, &registry, None)
            .expect("reloaded import graph must build through PresetRuntime::from_def");
    }

    /// IMPORT_FIDELITY_DESIGN.md D8/F-P5 round-trip gate: a `Blend` alpha_mode
    /// (from a transmission material) and its performer-facing Opacity card
    /// binding must survive save → reload, and stay live (modulatable) after
    /// reload — the BUG-036 rule (create-path green is half a gate for
    /// stateful features).
    #[test]
    fn round_trip_preserves_blend_alpha_mode_and_opacity_binding() {
        let mut m = full_material(0, "Windshield", 500);
        m.was_blend = true;
        m.transmission_factor = 0.9;
        let summary = GltfImportSummary {
            materials: vec![m],
            bbox_min: [-1.0, -1.0, -1.0],
            bbox_max: [1.0, 1.0, 1.0],
            camera_count: 0,
            default_material_vertex_count: 0,
        };
        let path = std::path::Path::new("/tmp/synthetic_glass_round_trip.glb");
        let (def, _report) = build_import_graph(&summary, path).expect("build graph");

        let json = serde_json::to_string(&def).expect("serialize EffectGraphDef");
        let reloaded: EffectGraphDef = serde_json::from_str(&json).expect("deserialize EffectGraphDef");
        assert_eq!(def, reloaded, "round trip must be byte-for-byte structurally identical");

        let flat = manifold_core::flatten::flatten_groups(&reloaded).expect("flatten reloaded def");
        let mat = flat
            .nodes
            .iter()
            .find(|n| n.type_id == "node.pbr_material")
            .expect("pbr_material node");
        assert_eq!(
            mat.params.get("alpha_mode"),
            Some(&enum_val(2)),
            "reloaded def must still carry alpha_mode Blend"
        );

        let meta = reloaded.preset_metadata.as_ref().expect("reloaded v2 metadata");
        assert!(
            meta.bindings.iter().any(|b| b.label == "Opacity"),
            "reloaded def must still carry the glass object's Opacity card binding"
        );

        // Modulation live after reload — not just structurally present.
        let registry = PrimitiveRegistry::with_builtin();
        PresetRuntime::from_def(reloaded, &registry, None)
            .expect("reloaded glass import graph must build through PresetRuntime::from_def");
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
            normal_texture: None,
            normal_scale: 1.0,
            mr_texture: None,
            occlusion_texture: None,
            occlusion_strength: 1.0,
            emissive_texture: None,
            emissive_strength: 1.0,
            ior: 1.5,
            specular_factor: 1.0,
            specular_color_factor: [1.0, 1.0, 1.0],
            specular_has_texture: false,
            base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
            uv_tex_coord_override: false,
            transmission_factor: 0.0,
            clearcoat_factor: 0.0,
            clearcoat_roughness_factor: 0.0,
            clearcoat_has_texture: false,
            was_blend: false,
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

    // Only used by the `#[cfg(feature = "gpu-proofs")]` render gates below —
    // gated the same way to avoid a dead-code warning on the default
    // (GPU-free) test sweep.
    #[cfg(feature = "gpu-proofs")]
    fn damaged_helmet_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/DamagedHelmet.glb")
    }

    /// Build a minimal in-memory glb: one XZ quad whose UVs live ENTIRELY
    /// outside [0,1] (V in [1.25, 1.75]) textured by an embedded 2×2 PNG —
    /// top row BLUE, bottom row RED. Under the glTF default sampler
    /// (REPEAT) V wraps to [0.25, 0.75] and samples the TOP (blue) row;
    /// under ClampToEdge every sample pins to the BOTTOM (red) row. The
    /// out-of-range-UV regression fixture for
    /// `material_maps_repeat_out_of_range_uvs`.
    #[cfg(feature = "gpu-proofs")]
    fn build_out_of_range_uv_glb() -> Vec<u8> {
        let positions: [[f32; 3]; 4] =
            [[-1.0, 0.0, -1.0], [1.0, 0.0, -1.0], [1.0, 0.0, 1.0], [-1.0, 0.0, 1.0]];
        let normals: [[f32; 3]; 4] = [[0.0, 1.0, 0.0]; 4];
        // V spans [1.05, 1.45]: REPEAT wraps to [0.05, 0.45] — entirely
        // inside the TOP (blue) row of the 2×2 texture. Clamp pins to
        // V = 1.0, the BOTTOM (red) edge row. (First cut used [1.25, 1.75],
        // which wraps across BOTH rows and reads mixed — a fixture bug,
        // not a sampler bug.)
        let uvs: [[f32; 2]; 4] = [[0.25, 1.05], [0.75, 1.05], [0.75, 1.45], [0.25, 1.45]];
        let indices: [u16; 6] = [0, 2, 1, 0, 3, 2];

        // 2×2 PNG: row 0 (top) blue, row 1 red.
        let mut png = Vec::new();
        {
            use image::ImageEncoder;
            let pixels: [u8; 16] =
                [0, 0, 255, 255, 0, 0, 255, 255, 255, 0, 0, 255, 255, 0, 0, 255];
            image::codecs::png::PngEncoder::new(&mut png)
                .write_image(&pixels, 2, 2, image::ExtendedColorType::Rgba8)
                .expect("encode fixture png");
        }

        let mut bin: Vec<u8> = Vec::new();
        let pos_off = bin.len();
        bin.extend(positions.iter().flatten().flat_map(|f| f.to_le_bytes()));
        let norm_off = bin.len();
        bin.extend(normals.iter().flatten().flat_map(|f| f.to_le_bytes()));
        let uv_off = bin.len();
        bin.extend(uvs.iter().flatten().flat_map(|f| f.to_le_bytes()));
        let idx_off = bin.len();
        bin.extend(indices.iter().flat_map(|i| i.to_le_bytes()));
        while bin.len() % 4 != 0 {
            bin.push(0);
        }
        let png_off = bin.len();
        bin.extend_from_slice(&png);
        while bin.len() % 4 != 0 {
            bin.push(0);
        }

        let json = serde_json::json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0}],
            "meshes": [{"primitives": [{
                "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
                "indices": 3,
                "material": 0
            }]}],
            "materials": [{"pbrMetallicRoughness": {
                "baseColorTexture": {"index": 0},
                "metallicFactor": 0.0,
                "roughnessFactor": 1.0
            }}],
            "textures": [{"source": 0, "sampler": 0}],
            "samplers": [{}],
            "images": [{"bufferView": 4, "mimeType": "image/png"}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3",
                 "min": [-1.0, 0.0, -1.0], "max": [1.0, 0.0, 1.0]},
                {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3"},
                {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC2"},
                {"bufferView": 3, "componentType": 5123, "count": 6, "type": "SCALAR"}
            ],
            "bufferViews": [
                {"buffer": 0, "byteOffset": pos_off, "byteLength": 48},
                {"buffer": 0, "byteOffset": norm_off, "byteLength": 48},
                {"buffer": 0, "byteOffset": uv_off, "byteLength": 32},
                {"buffer": 0, "byteOffset": idx_off, "byteLength": 12},
                {"buffer": 0, "byteOffset": png_off, "byteLength": png.len()}
            ],
            "buffers": [{"byteLength": bin.len()}]
        });
        let mut json_bytes = serde_json::to_vec(&json).expect("fixture json");
        while json_bytes.len() % 4 != 0 {
            json_bytes.push(b' ');
        }

        let total = 12 + 8 + json_bytes.len() + 8 + bin.len();
        let mut glb = Vec::with_capacity(total);
        glb.extend_from_slice(b"glTF");
        glb.extend_from_slice(&2u32.to_le_bytes());
        glb.extend_from_slice(&(total as u32).to_le_bytes());
        glb.extend_from_slice(&(json_bytes.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"JSON");
        glb.extend_from_slice(&json_bytes);
        glb.extend_from_slice(&(bin.len() as u32).to_le_bytes());
        glb.extend_from_slice(b"BIN\0");
        glb.extend_from_slice(&bin);
        glb
    }

    /// Regression gate for the 2026-07-15 striped-helmet bug: material maps
    /// must sample with REPEAT wrapping (the glTF default sampler), not the
    /// envmap's clamp-V. The fixture quad's V coords are entirely in
    /// [1.25, 1.75]: REPEAT reads the texture's blue top row; the broken
    /// clamp pinned every sample to the red bottom edge row. Asserts the
    /// rendered quad is blue-dominant. Run deliberately (gpu-proofs).
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn material_maps_repeat_out_of_range_uvs() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let glb = build_out_of_range_uv_glb();
        let path = std::env::temp_dir().join("manifold_uv_wrap_regression.glb");
        std::fs::write(&path, &glb).expect("write temp fixture");

        let (def, _report) = assemble_import_graph(&path).expect("assemble uv-wrap fixture");

        let (w, h) = (128u32, 128u32);
        let device = crate::test_device();
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator = PresetRuntime::from_def_with_device(
            def,
            &registry,
            device.arc(),
            w,
            h,
            GpuTextureFormat::Rgba16Float,
            None,
        )
        .expect("uv-wrap fixture builds through PresetRuntime");
        let target = RenderTarget::new(&device, w, h, GpuTextureFormat::Rgba16Float, "uv-wrap");
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

        // Poll until the background mesh+texture decodes land (byte-stable,
        // non-black — the BUG-100 double condition).
        let mut rgb_sum = [0.0f32; 3];
        let mut prev: Option<Vec<u8>> = None;
        let mut stable = 0u32;
        let mut converged = false;
        for _ in 0..200 {
            {
                let mut enc = device.create_encoder("uv-wrap-render");
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
            let buf = device.create_buffer_shared(u64::from(h * bytes_per_row));
            let mut renc = device.create_encoder("uv-wrap-readback");
            renc.copy_texture_to_buffer(&target.texture, &buf, w, h, bytes_per_row);
            renc.commit_and_wait_completed();
            let ptr = buf.mapped_ptr().expect("shared readback");
            let halves: &[u16] =
                unsafe { std::slice::from_raw_parts(ptr.cast::<u16>(), (w * h * 4) as usize) };
            rgb_sum = [0.0; 3];
            let mut raw = Vec::with_capacity(halves.len() * 2);
            for px in halves.chunks_exact(4) {
                rgb_sum[0] += half_to_f32(px[0]).max(0.0);
                rgb_sum[1] += half_to_f32(px[1]).max(0.0);
                rgb_sum[2] += half_to_f32(px[2]).max(0.0);
                raw.extend(px.iter().flat_map(|v| v.to_le_bytes()));
            }
            let non_black = rgb_sum.iter().sum::<f32>() > 1.0;
            if non_black && prev.as_deref() == Some(raw.as_slice()) {
                stable += 1;
            } else {
                stable = 0;
            }
            prev = Some(raw);
            if stable >= 3 {
                converged = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(converged, "uv-wrap fixture render never stabilized non-black");
        assert!(
            rgb_sum[2] > rgb_sum[0] * 2.0,
            "out-of-range V must WRAP to the blue top row, not clamp to the red \
             bottom edge: sum RGB = {rgb_sum:?}"
        );
    }

    #[cfg(feature = "gpu-proofs")]
    fn amg_gt3_fixture_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures/gltf/mercedes-amg_gt3__www.vecarz.com.glb")
    }

    /// F-P4 held-out-input gate (DESIGN_DOC_STANDARD.md §5): Khronos
    /// `DamagedHelmet.glb` (CC-BY 4.0 — attribution in
    /// `tests/fixtures/gltf/README.md`) carries all five glTF PBR map types
    /// and was never used to develop this importer — the fixture-overfitting
    /// check. Must import with every one of F-P2's four new map ports wired
    /// (asserted by port name), render headless through the real
    /// `PresetRuntime::from_def_with_device` + `render()` path without error,
    /// and produce a non-degenerate frame: mean luminance strictly between
    /// 0.02 and 0.98 (catches both an all-black failure — e.g. a stuck
    /// background decode — and a blown-out one — e.g. a light-leak from a
    /// broken IBL term — without judging the LOOK, which is Peter's L4 call).
    /// Needs a GPU device: run deliberately with `--features gpu-proofs`.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_core::flatten::flatten_groups;
        use manifold_gpu::GpuTextureFormat;

        let path = damaged_helmet_fixture_path();
        if !path.exists() {
            eprintln!(
                "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: fixture not \
                 found at {}, skipping",
                path.display()
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble DamagedHelmet");
        println!("DamagedHelmet import report: {report:?}");
        assert_eq!(report.object_count, 1, "DamagedHelmet is a single-material model");

        // Every one of F-P2's four new map ports must be wired, by name —
        // not just "the import didn't error".
        let flat = flatten_groups(&def).expect("DamagedHelmet import graph flattens");
        let render_ports: std::collections::HashSet<String> = flat
            .wires
            .iter()
            .filter(|w| {
                flat.nodes
                    .iter()
                    .any(|n| n.id == w.to_node && n.node_id.as_str() == "render")
            })
            .map(|w| w.to_port.clone())
            .collect();
        for port in ["base_color_map_0", "normal_map_0", "mr_map_0", "occlusion_map_0", "emissive_map_0"] {
            assert!(
                render_ports.contains(port),
                "DamagedHelmet must wire `{port}` — carries all five glTF PBR maps; got {render_ports:?}"
            );
        }

        let (w, h) = (512u32, 512u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("DamagedHelmet import graph must build through PresetRuntime::from_def_with_device");
        let target = RenderTarget::new(&device, w, h, format, "damaged-helmet");
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

        // Same convergence-polling loop as `imported_azalea_renders_faithfully_to_png`
        // — background texture/mesh decodes need to land before the readback
        // means anything. Byte-identical across STABLE_STREAK consecutive
        // frames is the completion signal, but (BUG-100) byte-identical
        // alone isn't enough: DamagedHelmet wires FIVE background texture
        // decodes (base-color/normal/mr/occlusion/emissive, each its own
        // `node.gltf_texture_source` background thread), and
        // `node.gltf_texture_source` emits solid black on every frame until
        // its own decode lands (see that primitive's `run()` step 6) — so a
        // frame where every wired source is STILL mid-decode is *also*
        // byte-stable (three identical black frames) and would falsely read
        // as "converged" before any decode actually finished. Require
        // `fraction > 0.02` (measured, non-black) alongside byte-stability,
        // exactly like the azalea proof's own convergence check.
        const STABLE_STREAK: u32 = 3;
        let max_attempts = 200;
        let mut rgba = Vec::new();
        let mut prev_rgba: Option<Vec<u8>> = None;
        let mut stable_count = 0u32;
        let mut converged = false;
        let mut fraction = 0.0f64;
        for attempt in 0..max_attempts {
            {
                let mut enc = device.create_encoder("damaged-helmet-render");
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
            let mut readback_enc = device.create_encoder("damaged-helmet-readback");
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
                    "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: converged \
                     on attempt {attempt} (non-black fraction {fraction:.4})"
                );
                converged = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        assert!(
            converged,
            "DamagedHelmet render never stabilized non-black after {max_attempts} attempts \
             (last non-black fraction {fraction:.4}) — a background texture decode may be stuck"
        );

        // Mean luminance (Rec. 601 luma over the tonemapped LDR frame),
        // strictly between 0.02 and 0.98 — non-degenerate without judging
        // the look.
        let mut sum_luma = 0.0f64;
        let pixel_count = (w * h) as usize;
        for px in rgba.chunks_exact(4) {
            let (r, g, b) = (px[0] as f64 / 255.0, px[1] as f64 / 255.0, px[2] as f64 / 255.0);
            sum_luma += 0.299 * r + 0.587 * g + 0.114 * b;
        }
        let mean_luminance = sum_luma / pixel_count as f64;
        println!(
            "damaged_helmet_imports_wires_all_maps_and_renders_non_degenerate: mean luminance = {mean_luminance:.4}"
        );

        let out_path = std::env::var("MESH_SNAP_OUT")
            .unwrap_or_else(|_| "target/mesh-snap/damaged_helmet.png".to_string());
        if let Some(parent) = std::path::Path::new(&out_path).parent() {
            std::fs::create_dir_all(parent).expect("create output dir");
        }
        image::save_buffer(&out_path, &rgba, w, h, image::ExtendedColorType::Rgba8)
            .unwrap_or_else(|e| panic!("save {out_path}: {e}"));

        assert!(
            mean_luminance.is_finite() && mean_luminance > 0.02 && mean_luminance < 0.98,
            "expected non-degenerate mean luminance in (0.02, 0.98), got {mean_luminance:.4}"
        );
    }

    /// Peter-facing look-check sanity, NOT a machine gate (the AMG fixture
    /// is untracked, licensing-unverified — vecarz — and absent in a fresh
    /// checkout, per the design's explicit "stays untracked" call). Skips
    /// cleanly when the file is absent so it never fails CI; when present,
    /// only proves the import + render pipeline doesn't error — the actual
    /// look (chrome + void + glow) is Peter's in-app L4 check.
    #[cfg(feature = "gpu-proofs")]
    #[test]
    fn amg_gt3_glb_imports_and_renders_without_error_if_present() {
        use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
        use crate::preset_context::PresetContext;
        use crate::render_target::RenderTarget;
        use manifold_gpu::GpuTextureFormat;

        let path = amg_gt3_fixture_path();
        if !path.exists() {
            println!(
                "amg_gt3_glb_imports_and_renders_without_error_if_present: fixture not tracked, \
                 skipping (expected in a fresh checkout — vecarz licensing unverified)"
            );
            return;
        }

        let (def, report) = assemble_import_graph(&path).expect("assemble AMG GT3");
        println!("AMG GT3 import report: {report:?}");
        // GLB_CONFORMANCE_DESIGN.md G-P2 conformance gate: the AMG GT3 has
        // 78 materials with geometry; with the cap dead, ALL of them must be
        // wired (pre-G-P2 this was 64, dropping the livery — BUG-163).
        assert_eq!(
            report.object_count, 78,
            "AMG GT3 import must be 1:1 — 78 materials, 78 objects, no cap-drop (BUG-163)"
        );
        assert_eq!(report.material_count, 78);

        let (w, h) = (512u32, 512u32);
        let device = crate::test_device();
        let format = GpuTextureFormat::Rgba16Float;
        let registry = PrimitiveRegistry::with_builtin();
        let mut generator =
            PresetRuntime::from_def_with_device(def, &registry, device.arc(), w, h, format, None)
                .expect("AMG GT3 import graph must build through PresetRuntime::from_def_with_device");
        let target = RenderTarget::new(&device, w, h, format, "amg-gt3-sanity");
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
        let mut enc = device.create_encoder("amg-gt3-sanity-render");
        {
            let mut gpu = RendererGpuEncoder::new(&mut enc, &device);
            generator.render(&mut gpu, &target.texture, &ctx, &manifold_core::params::ParamManifest::default());
        }
        enc.commit_and_wait_completed();
        println!("amg_gt3_glb_imports_and_renders_without_error_if_present: rendered without error");
    }
}

