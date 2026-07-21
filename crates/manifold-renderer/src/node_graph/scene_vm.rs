//! `SceneVm` — the Scene Setup Panel's sole discovery mechanism
//! (`docs/SCENE_SETUP_PANEL_DESIGN.md` D3, object model per
//! `docs/SCENE_OBJECT_AND_PANEL_V2_DESIGN.md` D12).
//!
//! Pure function of an [`EffectGraphDef`]: no GPU, no registry lookups
//! beyond type_id string comparisons, no reads of the project model. Every editable
//! row carries its write address — `(scope_path, node_doc_id, param_id)`
//! — the exact addressing [`manifold_editing::commands::graph::SetGraphNodeParamCommand`]
//! (`.with_scope`) takes, so the panel dispatches through the identical
//! command the graph editor's node face already uses (the "fourth
//! surface" — card, node face, group face, and now the dock).
//!
//! Curated + tolerant, per D3: known shapes (the importer's environment
//! chain, `node.light`, the three camera atoms, `node.atmosphere`,
//! `node.scene_object`) get editable rows; anything else degrades to an
//! honest labeled "custom" row. Nothing is ever hidden and nothing errors.
//!
//! D12: object discovery anchors on `node.scene_object` — the sole producer
//! of an `Object` wire (SCENE_OBJECT_AND_PANEL_V2_DESIGN D1's single-hop
//! invariant) — found either directly wired to `render_scene`'s `object_k`
//! port (a hand-built, ungrouped object) or through one `GROUP_TYPE_ID`
//! wrapper whose body contains the `node.scene_object` feeding a
//! `system.group_output`'s `object` port (the importer/`AddSceneObjectCommand`
//! shape). The old "whatever named group happens to wrap `mesh_k`" trace is
//! gone — `render_scene` v2 has no `mesh_k`/`transform_k`/… port families
//! left to look at.
//!
//! Rebuilt from scratch on every `state_sync` pass — no cached/staged
//! copy anywhere (Peter: "no rotting, no staleness"). See the D3 "plausible
//! wrong architecture" callout: this module must never grow a persistent
//! mirror of scene values.

use std::collections::HashSet;

use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, SerializedParamValue,
};

use crate::node_graph::FINAL_OUTPUT_TYPE_ID;

/// `node.render_scene`'s own type_id string (curated vocabulary anchor).
pub const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";
/// `node.scene_object`'s own type_id string — the sole `Object`-wire
/// producer (SCENE_OBJECT_AND_PANEL_V2_DESIGN D1/D12).
const SCENE_OBJECT_TYPE_ID: &str = "node.scene_object";
const LIGHT_TYPE_ID: &str = "node.light";
const ATMOSPHERE_TYPE_ID: &str = "node.atmosphere";
const BAKE_ENVIRONMENT_TYPE_ID: &str = "node.bake_environment";
const HDRI_SOURCE_TYPE_ID: &str = "node.hdri_source";
const EXPOSURE_TYPE_ID: &str = "node.exposure";
const SWITCH_TEXTURE_TYPE_ID: &str = "node.switch_texture";
const TRANSFORM_3D_TYPE_ID: &str = "node.transform_3d";
const ORBIT_CAMERA_TYPE_ID: &str = "node.orbit_camera";
const FREE_CAMERA_TYPE_ID: &str = "node.free_camera";
const LOOK_AT_CAMERA_TYPE_ID: &str = "node.look_at_camera";
const CAMERA_LENS_TYPE_ID: &str = "node.camera_lens";
/// PBR/phong/unlit/cel — the four material atoms (D3's Objects material row).
const MATERIAL_TYPE_IDS: &[&str] = &[
    "node.pbr_material",
    "node.phong_material",
    "node.unlit_material",
    "node.cel_material",
];
/// The curated mesh-modifier vocabulary (D6): single-mesh-in/mesh-out atoms.
const MODIFIER_TYPE_IDS: &[&str] = &[
    "node.bend_mesh",
    "node.twist_mesh",
    "node.taper_mesh",
    "node.push_along_normals",
    "node.push_mesh",
    "node.morph_mesh",
    "node.rotate_3d",
];
/// A write address for one editable value: the exact addressing
/// `SetGraphNodeParamCommand::with_scope` takes.
#[derive(Debug, Clone, PartialEq)]
pub struct ParamAddr {
    pub scope_path: Vec<u32>,
    pub node_doc_id: u32,
    pub param_id: String,
}

/// Full-panel discovery result for one generator layer's graph.
#[derive(Debug, Clone, PartialEq)]
pub struct SceneVm {
    /// Doc id of the chosen `node.render_scene` (first by id among reachable
    /// candidates when more than one is live).
    pub scene_root_node_id: u32,
    /// `true` when more than one live `render_scene` was found — the panel
    /// shows a static "N scenes in this graph — showing the first" chip.
    pub multiple_scenes: bool,
    pub header: SceneHeaderVm,
    pub objects: Vec<SceneObjectVm>,
    pub lights: Vec<SceneLightVm>,
    pub camera: CameraVm,
    pub environment: EnvironmentVm,
    pub atmosphere: AtmosphereVm,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SceneHeaderVm {
    pub object_count: usize,
    pub light_count: usize,
    pub shadow_caster_count: usize,
    /// BUG-194: sum of every resolved object's mesh-source vertex count —
    /// import-time provenance on `node.gltf_mesh_source` /
    /// `node.gltf_skinned_mesh_source` (`source_vertex_count` param) plus a
    /// closed-form table for the trivially-computable procedural generators
    /// (`node.cube_mesh`, `node.grid_mesh`). Honest, not a fabricated
    /// proxy — see `vertex_count_exact`.
    pub vertex_count: u64,
    /// `false` when at least one object's mesh source didn't resolve to a
    /// known count (an unmapped procedural generator, an unparseable
    /// modifier chain, a malformed def) — the panel must render this as
    /// "≥ N", never a bare "N", when `false`.
    pub vertex_count_exact: bool,
}

/// Payload for [`SceneObjectVm::Known`], boxed at the enum site for the same
/// clippy `large_enum_variant` reason as [`LightRow`]/[`OrbitCameraRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct SceneObjectKnownRow {
    pub index: usize,
    /// The `node.scene_object`'s own doc id — the address
    /// `RenameSceneObjectCommand`/the eye-toggle write take, and the same
    /// value `group_node_id` resolved to pre-D12 when an object happened to
    /// be grouped.
    pub object_node_id: u32,
    /// `Some(group_id)` when the scene_object is wrapped in a
    /// `GROUP_TYPE_ID` node (the importer/`AddSceneObjectCommand` shape) —
    /// the rename sweep's group target. `None` for a bare scene_object
    /// wired directly to `object_k` (D1's first-class "hand-built graph, no
    /// group" case).
    pub group_node_id: Option<u32>,
    pub name: String,
    pub visible_addr: ParamAddr,
    pub visible_value: bool,
    /// `true` when a wire feeds `visible` directly (the primitive's
    /// port-shadow convention) — the panel renders the eye toggle read-only
    /// with the "driven" styling, same as every other `_driven` field in
    /// this module.
    pub visible_driven: bool,
    pub transform: Option<TransformVm>,
    pub material: MaterialVm,
    /// Chain of single-mesh-input/mesh-output nodes between the mesh source
    /// and the scene_object's `vertices` input, in wire order (D6's
    /// modifier stack, re-anchored per D12).
    pub modifier_chain: Vec<ModifierVm>,
    /// `false` when the scene_object's `vertices` chain couldn't be walked
    /// at all (an unwired `vertices` port, a dangling wire, or a cycle) —
    /// P5's "custom chain — edit in graph" case, DISTINCT from a
    /// well-formed stack that's simply empty (a fresh object with zero
    /// modifiers: `vertices` resolves straight to the mesh source,
    /// `modifier_chain` is `[]` and this is `true`). The panel disables
    /// "Add modifier" only when this is `false` — never a blind splice
    /// into unrecognized topology (D6).
    pub modifier_chain_parseable: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SceneObjectVm {
    /// Producer resolved to a `node.scene_object` (D12), directly or through
    /// one wrapping group.
    Known(Box<SceneObjectKnownRow>),
    /// Producer did NOT resolve to a `node.scene_object` — "Object k —
    /// custom (edit in graph)" per D3, degraded but never hidden.
    Custom { index: usize },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModifierVm {
    pub node_doc_id: u32,
    pub type_id: String,
}

/// One `node.transform_3d`'s write addresses + current values — D4's "3
/// compact triplets" (Position/Rotation/Scale), each X/Y/Z. Traced
/// independently of the object's group (the importer/`AddSceneObjectCommand`
/// shape places it in the same group by convention, but the trace never
/// assumes that — a hand-wired `transform` input still resolves here as long
/// as the producer IS a `node.transform_3d`).
#[derive(Debug, Clone, PartialEq)]
pub struct TransformVm {
    pub node_doc_id: u32,
    pub pos_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub pos_value: (f32, f32, f32),
    /// Per-axis: `true` when a wire feeds that axis directly (the
    /// primitive's port-shadow convention) — the panel renders that axis
    /// read-only with the "driven" styling (D4), never fighting the graph.
    pub pos_driven: (bool, bool, bool),
    pub rot_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub rot_value: (f32, f32, f32),
    pub rot_driven: (bool, bool, bool),
    pub scale_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub scale_value: (f32, f32, f32),
    pub scale_driven: (bool, bool, bool),
}

/// Payload for [`MaterialVm::Known`], boxed for the same reason as
/// [`LightRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct MaterialColorRow {
    pub node_doc_id: u32,
    /// The scope this material atom's params write at — empty for a root/
    /// ungrouped object, `[group_node_id]` for one living inside an object's
    /// group (or, on the rare crossed-group shape, one level deeper). Kept
    /// as addressing identity (not a param value) — the sole source of the
    /// scope once `base_color_addr` no longer exists.
    pub scope_path: Vec<u32>,
    /// `true` only for `node.pbr_material` — metallic/roughness is a
    /// PBR-only concept, so a phong/unlit/cel material's quick knobs are
    /// base color alone (D4: "the atom's own params otherwise").
    pub is_pbr: bool,
}

/// The Objects section's material quick-knob row (D3/D4).
#[derive(Debug, Clone, PartialEq)]
pub enum MaterialVm {
    Known(Box<MaterialColorRow>),
    /// No material resolved (unwired `material` port, or a producer that
    /// isn't one of the four curated material atoms).
    None,
}

/// Payload for [`SceneLightVm::Known`], boxed at the enum site so the enum's
/// footprint tracks the small `Custom` variant instead of this one
/// (clippy `large_enum_variant`). Carries both the write address AND the
/// CURRENT value for every row (same convention as [`ImporterEnvironmentRow`]
/// / [`AtmosphereRow`]) — the panel renders sliders/steppers, which need a
/// live position, not just a target. `mode`/`shadow_softness` are the
/// primitive's Enum-typed params (`node.light`'s `LIGHT_MODES` /
/// `SHADOW_SOFTNESS_LABELS`); their value is the raw enum index — the panel
/// owns the display-label mapping (it can't depend on this crate's
/// constants, same DTO-boundary convention as `EnvironmentRowVm::mode_is_hdri`).
/// `light_size` is P9's Contact-softness-only knob (REALTIME_3D_DESIGN.md) —
/// always resolved and always writable (D4: "parameter dependency, not
/// conditional UI"), regardless of the current `shadow_softness_value`.
#[derive(Debug, Clone, PartialEq)]
pub struct LightRow {
    pub index: usize,
    pub node_doc_id: u32,
    /// P5: the light's display name — its own `handle`, falling back to
    /// `"Light {k}"` (same convention as an object's name, D6). NEW: lights
    /// didn't have an editable display name before this design.
    pub name: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SceneLightVm {
    Known(Box<LightRow>),
    Custom { index: usize },
}

/// `node.camera_lens`'s four port-shadowed scalar params (D3: "the lens
/// node's own row beneath"), with the same address+value+driven shape as
/// every other editable row. `None` on a camera row means no lens node was
/// traced between the camera atom and `render_scene`'s `camera` port — the
/// importer's shape always inserts one, but a hand-wired camera may not.
#[derive(Debug, Clone, PartialEq)]
pub struct LensRow {
    pub node_doc_id: u32,
}

/// Payload for [`CameraVm::Orbit`], boxed for the same reason as
/// [`LightRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct OrbitCameraRow {
    pub node_doc_id: u32,
    pub lens: Option<LensRow>,
}

/// Payload for [`CameraVm::Free`] (D3: "free: pos/euler/fov rows").
#[derive(Debug, Clone, PartialEq)]
pub struct FreeCameraRow {
    pub node_doc_id: u32,
    pub lens: Option<LensRow>,
}

/// Payload for [`CameraVm::LookAt`] (D3: "look-at: pos/target/fov rows").
#[derive(Debug, Clone, PartialEq)]
pub struct LookAtCameraRow {
    pub node_doc_id: u32,
    pub lens: Option<LensRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CameraVm {
    None,
    Orbit(Box<OrbitCameraRow>),
    Free(Box<FreeCameraRow>),
    LookAt(Box<LookAtCameraRow>),
    Custom { node_doc_id: u32 },
}

/// Payload for [`EnvironmentVm::Importer`] (boxed — see [`LightRow`]).
/// Carries both the write address AND the CURRENT value for each row: the
/// panel renders sliders, which need a live position, not just a target.
#[derive(Debug, Clone, PartialEq)]
pub struct ImporterEnvironmentRow {
    /// The `node.switch_texture` selector's own doc id (the "mode" chip).
    pub switch_node_id: u32,
    /// The `node.bake_environment`'s doc id (the Softbox intensity/fill
    /// params).
    pub bake_node_id: u32,
    /// The `node.hdri_source`'s doc id — its `path` param isn't manifest/
    /// slider-backed (a file path, not a numeric row), so only the resolved
    /// display string is carried, not an addr.
    pub hdri_node_id: u32,
    pub hdri_file_value: String,
}

/// Payload for [`EnvironmentVm::Bare`].
#[derive(Debug, Clone, PartialEq)]
pub struct BareEnvironmentRow {
    pub node_doc_id: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum EnvironmentVm {
    /// The importer's shape: `switch_texture` selecting between
    /// `bake_environment` (Softbox) and `hdri_source`→`exposure` (HDRI).
    Importer(Box<ImporterEnvironmentRow>),
    /// A bare `node.bake_environment`, no HDRI switch.
    Bare(Box<BareEnvironmentRow>),
    /// Some other producer wired into `envmap` — honest custom row.
    Custom { node_doc_id: u32 },
    /// `envmap` unwired — D3's "Add environment" action.
    None,
}

/// Payload for [`AtmosphereVm::Wired`], boxed for the same reason as
/// [`LightRow`]. Carries current values alongside each write address (see
/// [`ImporterEnvironmentRow`]).
#[derive(Debug, Clone, PartialEq)]
pub struct AtmosphereRow {
    pub node_doc_id: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub enum AtmosphereVm {
    Wired(Box<AtmosphereRow>),
    /// `atmosphere` unwired — D3's "Add fog" action.
    None,
}

/// Minimal view of one node + its incoming wires, scoped to a single graph
/// level (root or inside a group) — the trace never crosses a group
/// boundary itself.
struct Level<'a> {
    nodes: &'a [manifold_core::effect_graph_def::EffectGraphNode],
    wires: &'a [manifold_core::effect_graph_def::EffectGraphWire],
}

impl<'a> Level<'a> {
    fn node(&self, id: u32) -> Option<&'a EffectGraphNode> {
        self.nodes.iter().find(|n| n.id == id)
    }

    /// The (node, port) feeding `to_node`'s `to_port`, if wired.
    fn producer(&self, to_node: u32, to_port: &str) -> Option<(u32, &'a str)> {
        self.wires
            .iter()
            .find(|w| w.to_node == to_node && w.to_port == to_port)
            .map(|w| (w.from_node, w.from_port.as_str()))
    }
}

fn param_f32(node: &EffectGraphNode, name: &str, default: f32) -> f32 {
    match node.params.get(name) {
        Some(SerializedParamValue::Float { value }) => *value,
        Some(SerializedParamValue::Int { value }) => *value as f32,
        Some(SerializedParamValue::Enum { value }) => *value as f32,
        _ => default,
    }
}

impl SceneVm {
    /// Discover the full D3 trace for `def`. `None` when no `render_scene`
    /// reaches the graph's output (the empty-scene case; D7 handles it).
    pub fn from_def(def: &EffectGraphDef) -> Option<SceneVm> {
        let root = Level { nodes: &def.nodes, wires: &def.wires };

        // Liveness: reachable-from-output, walked backward from
        // `system.final_output`'s `in` wire — the same liveness notion the
        // canvas already computes (D3).
        let sink = root.nodes.iter().find(|n| n.type_id == FINAL_OUTPUT_TYPE_ID)?;
        let reachable = reachable_backward(&root, sink.id);

        let mut candidates: Vec<u32> = root
            .nodes
            .iter()
            .filter(|n| n.type_id == RENDER_SCENE_TYPE_ID && reachable.contains(&n.id))
            .map(|n| n.id)
            .collect();
        candidates.sort_unstable();
        let scene_root_node_id = *candidates.first()?;
        let multiple_scenes = candidates.len() > 1;
        let scene_node = root.node(scene_root_node_id)?;

        let (objects, vertex_count, vertex_count_exact) = trace_objects(&root, scene_node);
        let lights = trace_lights(&root, scene_node);
        let camera = trace_camera(&root, scene_node);
        let environment = trace_environment(&root, scene_node);
        let atmosphere = trace_atmosphere(&root, scene_node);

        let object_count = objects.len();
        let light_count = lights.len();
        let shadow_caster_count = lights.iter().filter(|l| light_casts_shadows(&root, l)).count();

        Some(SceneVm {
            scene_root_node_id,
            multiple_scenes,
            header: SceneHeaderVm {
                object_count,
                light_count,
                shadow_caster_count,
                vertex_count,
                vertex_count_exact,
            },
            objects,
            lights,
            camera,
            environment,
            atmosphere,
        })
    }
}

fn light_casts_shadows(level: &Level, light: &SceneLightVm) -> bool {
    let SceneLightVm::Known(row) = light else { return false };
    let Some(node) = level.node(row.node_doc_id) else { return false };
    param_f32(node, "cast_shadows", 0.0) > 0.5
}

/// BFS backward over wires from `start` (inclusive), within one graph level.
fn reachable_backward(level: &Level, start: u32) -> HashSet<u32> {
    let mut seen = HashSet::new();
    let mut stack = vec![start];
    seen.insert(start);
    while let Some(n) = stack.pop() {
        for w in level.wires.iter().filter(|w| w.to_node == n) {
            if seen.insert(w.from_node) {
                stack.push(w.from_node);
            }
        }
    }
    seen
}

/// D12: find the `node.scene_object` bound inside `group`'s body — the
/// producer of its `system.group_output`'s `object` port. Returns the node
/// plus a [`Level`] scoped to the group's own nodes/wires (needed for every
/// further trace INSIDE that scope). `None` when the group doesn't have this
/// shape at all (an unparseable/hand-edited group — the caller degrades to
/// `Custom`, never errors).
fn find_scene_object_in_group<'a>(
    group: &'a manifold_core::effect_graph_def::GroupDef,
) -> Option<(&'a EffectGraphNode, Level<'a>)> {
    let inner = Level { nodes: &group.nodes, wires: &group.wires };
    let out_node = inner.nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?;
    let (producer_id, _) = inner.producer(out_node.id, "object")?;
    let node = inner.node(producer_id)?;
    (node.type_id == SCENE_OBJECT_TYPE_ID).then_some((node, inner))
}

/// Resolve `to_node`'s `to_port` producer, transparently crossing ONE level
/// of `GROUP_TYPE_ID` boundary when the direct producer is a bare group
/// re-exporting the same-named port from its own `system.group_output` —
/// the shape `migrate_scene_object_wires` produces for every pre-existing
/// (migrated) project: the minted `node.scene_object` stays a ROOT-level
/// sibling of the mesh producer's group rather than nested inside it (D5's
/// "same-scope re-point", confirmed against the shipped `SceneStarter.json`:
/// `scene_object` id 32/33 at root, `vertices` wired straight from group
/// node 10/20's own boundary port). Returns the [`Level`] the resolved node
/// actually lives in (root, or the crossed group's own body — callers must
/// use THIS level for any further tracing) plus the crossed group's node id
/// (for `ParamAddr::scope_path`, `None` when no crossing happened). `None`
/// when unwired, or when a bare group doesn't have the expected
/// group-output/re-export shape — the caller degrades to `Custom`/absent,
/// never errors (same tolerance doctrine as `find_scene_object_in_group`).
fn resolve_producer_through_group<'a>(
    level: &Level<'a>,
    to_node: u32,
    to_port: &str,
) -> Option<(Level<'a>, Option<u32>, &'a EffectGraphNode, &'a str)> {
    let (producer_id, producer_port) = level.producer(to_node, to_port)?;
    let producer = level.node(producer_id)?;
    if producer.type_id != GROUP_TYPE_ID {
        return Some((Level { nodes: level.nodes, wires: level.wires }, None, producer, producer_port));
    }
    let group = producer.group.as_ref()?;
    let inner = Level { nodes: &group.nodes, wires: &group.wires };
    let out_node = inner.nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)?;
    let (inner_producer_id, inner_producer_port) = inner.producer(out_node.id, producer_port)?;
    let inner_producer = inner.node(inner_producer_id)?;
    Some((inner, Some(producer_id), inner_producer, inner_producer_port))
}

/// BUG-194 (D4) + D12: alongside the objects themselves, returns the summed
/// vertex count and whether that sum is exact (`true`) or a lower bound
/// (`false` — at least one object's mesh source didn't resolve to a known
/// count, e.g. a hand-wired procedural generator outside the closed-form
/// table, or an unparseable chain).
fn trace_objects(level: &Level, scene_node: &EffectGraphNode) -> (Vec<SceneObjectVm>, u64, bool) {
    let objects = param_f32(scene_node, "objects", 0.0).max(0.0) as usize;
    let mut vertex_count: u64 = 0;
    let mut vertex_count_exact = true;
    let mut out = Vec::with_capacity(objects);
    for k in 0..objects {
        let port = format!("object_{k}");
        let (row, source_vertex_count) = match level.producer(scene_node.id, &port) {
            Some((producer_id, _)) => match level.node(producer_id) {
                Some(producer_node) if producer_node.type_id == SCENE_OBJECT_TYPE_ID => {
                    trace_scene_object(level, Vec::new(), producer_node, None, k)
                }
                Some(producer_node) if producer_node.type_id == GROUP_TYPE_ID => {
                    match producer_node.group.as_deref().and_then(find_scene_object_in_group) {
                        Some((inner_node, inner_level)) => {
                            trace_scene_object(
                                &inner_level,
                                vec![producer_id],
                                inner_node,
                                Some(producer_id),
                                k,
                            )
                        }
                        None => (SceneObjectVm::Custom { index: k }, None),
                    }
                }
                _ => (SceneObjectVm::Custom { index: k }, None),
            },
            None => (SceneObjectVm::Custom { index: k }, None),
        };
        match source_vertex_count {
            Some(v) => vertex_count += v as u64,
            None => vertex_count_exact = false,
        }
        out.push(row);
    }
    (out, vertex_count, vertex_count_exact)
}

/// Traces one `node.scene_object`'s full editable surface (D12): name,
/// visible, transform, material, modifier chain, map-presence — everything
/// addressed at `scope_path` (empty for a bare/ungrouped scene_object,
/// `[group_node_id]` when wrapped). Returns the `Known` row plus this
/// object's resolved mesh-source vertex count (`None` = unknown, BUG-194).
fn trace_scene_object(
    level: &Level,
    scope_path: Vec<u32>,
    node: &EffectGraphNode,
    group_node_id: Option<u32>,
    k: usize,
) -> (SceneObjectVm, Option<u32>) {
    let object_node_id = node.id;
    let name = node.handle.clone().unwrap_or_else(|| format!("Object {k}"));
    let visible_addr =
        ParamAddr { scope_path: scope_path.clone(), node_doc_id: object_node_id, param_id: "visible".to_string() };
    let visible_value = param_f32(node, "visible", 1.0) > 0.5;
    let visible_driven = level.producer(object_node_id, "visible").is_some();

    let transform = resolve_producer_through_group(level, object_node_id, "transform")
        .filter(|(_, _, n, _)| n.type_id == TRANSFORM_3D_TYPE_ID)
        .map(|(lvl, crossed_group, n, _)| {
            let mut sp = scope_path.clone();
            if let Some(g) = crossed_group {
                sp.push(g);
            }
            trace_transform(&lvl, sp, n.id)
        });

    let material = resolve_producer_through_group(level, object_node_id, "material")
        .filter(|(_, _, n, _)| MATERIAL_TYPE_IDS.contains(&n.type_id.as_str()))
        .map(|(_lvl, crossed_group, n, _)| {
            let mut scope_path = scope_path.clone();
            if let Some(g) = crossed_group {
                scope_path.push(g);
            }
            MaterialVm::Known(Box::new(MaterialColorRow {
                node_doc_id: n.id,
                scope_path,
                is_pbr: n.type_id == "node.pbr_material",
            }))
        })
        .unwrap_or(MaterialVm::None);

    // Modifier chain (D6, re-anchored per D12): walk backward from the
    // scene_object's OWN `vertices` input instead of a group output's
    // `vertices` port. `current_level` can switch mid-walk when the chain
    // crosses a bare-group boundary (the migrated-project shape,
    // `resolve_producer_through_group`'s doc comment) — a group is always
    // treated as a transparent re-export, never a modifier itself.
    let mut chain = Vec::new();
    let mut current_level = Level { nodes: level.nodes, wires: level.wires };
    let mut cursor = current_level.producer(object_node_id, "vertices");
    let mut parseable = cursor.is_some();
    let mut source_vertex_count: Option<u32> = None;
    let mut guard = 0;
    while let Some((node_id, _port)) = cursor {
        guard += 1;
        if guard > 64 {
            parseable = false; // cycle guard — never hang the panel on malformed JSON.
            break;
        }
        let Some(n) = current_level.node(node_id) else {
            parseable = false; // dangling wire — genuinely malformed.
            break;
        };
        if n.type_id == GROUP_TYPE_ID {
            let Some(group) = n.group.as_ref() else {
                parseable = false;
                break;
            };
            let inner = Level { nodes: &group.nodes, wires: &group.wires };
            let Some(out_node) = inner.nodes.iter().find(|gn| gn.type_id == GROUP_OUTPUT_TYPE_ID)
            else {
                parseable = false;
                break;
            };
            let Some((inner_id, inner_port)) = inner.producer(out_node.id, "vertices") else {
                parseable = false;
                break;
            };
            current_level = inner;
            cursor = Some((inner_id, inner_port));
            continue;
        }
        if !MODIFIER_TYPE_IDS.contains(&n.type_id.as_str()) {
            source_vertex_count = node_source_vertex_count(n);
            break; // reached the mesh source (or something un-curated) — stop, still parseable.
        }
        chain.push(ModifierVm { node_doc_id: n.id, type_id: n.type_id.clone() });
        cursor = current_level.producer(n.id, "in");
    }
    chain.reverse(); // wire order: source → … → scene_object.

    let row = SceneObjectVm::Known(Box::new(SceneObjectKnownRow {
        index: k,
        object_node_id,
        group_node_id,
        name,
        visible_addr,
        visible_value,
        visible_driven,
        transform,
        material,
        modifier_chain: chain,
        modifier_chain_parseable: parseable,
    }));
    (row, source_vertex_count)
}

/// Traces one `node.transform_3d`'s nine params at `level` into a full
/// [`TransformVm`], addressed with `scope_path` — empty when the atom lives
/// at root scope (a bare/ungrouped object), `[group_node_id]` when it lives
/// inside an object's group (the importer's shape).
fn trace_transform(level: &Level, scope_path: Vec<u32>, node_id: u32) -> TransformVm {
    let node = level.node(node_id);
    let pf = |name: &str, default: f32| node.map_or(default, |n| param_f32(n, name, default));
    let driven = |name: &str| level.producer(node_id, name).is_some();
    let addr = |s: &Vec<u32>, name: &str| ParamAddr { scope_path: s.clone(), node_doc_id: node_id, param_id: name.to_string() };
    TransformVm {
        node_doc_id: node_id,
        pos_addr: (addr(&scope_path, "pos_x"), addr(&scope_path, "pos_y"), addr(&scope_path, "pos_z")),
        pos_value: (pf("pos_x", 0.0), pf("pos_y", 0.0), pf("pos_z", 0.0)),
        pos_driven: (driven("pos_x"), driven("pos_y"), driven("pos_z")),
        rot_addr: (addr(&scope_path, "rot_x"), addr(&scope_path, "rot_y"), addr(&scope_path, "rot_z")),
        rot_value: (pf("rot_x", 0.0), pf("rot_y", 0.0), pf("rot_z", 0.0)),
        rot_driven: (driven("rot_x"), driven("rot_y"), driven("rot_z")),
        scale_addr: (addr(&scope_path, "scale_x"), addr(&scope_path, "scale_y"), addr(&scope_path, "scale_z")),
        scale_value: (pf("scale_x", 1.0), pf("scale_y", 1.0), pf("scale_z", 1.0)),
        scale_driven: (driven("scale_x"), driven("scale_y"), driven("scale_z")),
    }
}

/// BUG-194 (SCENE_SETUP_PANEL_DESIGN.md D4): read a mesh-source (or
/// closed-form procedural generator) node's vertex count, honestly. `None`
/// means unknown — the caller must never fabricate a number, it degrades the
/// header to "≥ N" instead.
fn node_source_vertex_count(node: &EffectGraphNode) -> Option<u32> {
    match node.type_id.as_str() {
        // `node.gltf_mesh_source` / `node.gltf_skinned_mesh_source`: import-time
        // provenance, a declared param (`source_vertex_count`, default -1 =
        // unknown) stamped by `gltf_import.rs` at import/merge time — see
        // those primitives' `params` block.
        "node.gltf_mesh_source" | "node.gltf_skinned_mesh_source" => {
            let v = param_f32(node, "source_vertex_count", -1.0);
            if v >= 0.0 { Some(v.round() as u32) } else { None }
        }
        _ => procedural_vertex_count(node),
    }
}

/// Closed-form vertex counts for procedural mesh generators whose output
/// size is a pure function of their own declared params — no GPU readback,
/// no fabricated numbers. §2.5 audit of every `Array(MeshVertex)`-producing
/// `Source`-role primitive: only `node.cube_mesh` (a fixed 36-vertex
/// constant — 6 faces × 2 triangles × 3 vertices, `generate_cube_mesh.rs`)
/// and `node.grid_mesh` (`resolution_x * resolution_y`, confirmed against
/// `generate_grid_mesh_body.wgsl`'s own index math) are trivially
/// closed-form; the rest (`node.revolve_curve`, `node.extrude_curve`,
/// `node.tube_from_path`, `node.platonic_solid_points`, …) depend on curve
/// length, topology tables, or a dynamically wired selector — genuinely not
/// computable from static params alone, so they fall through to `None`.
fn procedural_vertex_count(node: &EffectGraphNode) -> Option<u32> {
    match node.type_id.as_str() {
        "node.cube_mesh" => Some(36),
        "node.grid_mesh" => {
            let res_x = param_f32(node, "resolution_x", 256.0).max(2.0).round() as u32;
            let res_y = param_f32(node, "resolution_y", 256.0).max(2.0).round() as u32;
            Some(res_x * res_y)
        }
        _ => None,
    }
}

fn trace_lights(level: &Level, scene_node: &EffectGraphNode) -> Vec<SceneLightVm> {
    let lights = param_f32(scene_node, "lights", 0.0).max(0.0) as usize;
    (0..lights)
        .map(|k| {
            let port = format!("light_{k}");
            match level.producer(scene_node.id, &port) {
                Some((node_id, _)) if level.node(node_id).is_some_and(|n| n.type_id == LIGHT_TYPE_ID) => {
                    let node = level.node(node_id).expect("checked above");
                    SceneLightVm::Known(Box::new(LightRow {
                        index: k,
                        node_doc_id: node_id,
                        name: node.handle.clone().unwrap_or_else(|| format!("Light {k}")),
                    }))
                }
                _ => SceneLightVm::Custom { index: k },
            }
        })
        .collect()
}

/// Builds a [`LensRow`] for `node.camera_lens` at `node_id` — identity only;
/// its four port-shadowed scalar params (focus_distance/f_stop/shutter_angle/
/// exposure_ev) are read generically through `state_sync`'s manifest closures
/// keyed on this node id (D3's "the lens node's own row beneath").
fn trace_lens(level: &Level, node_id: u32) -> Option<LensRow> {
    level.node(node_id)?;
    Some(LensRow { node_doc_id: node_id })
}

/// Trace THROUGH single-camera-in/camera-out nodes (the importer's
/// `node.camera_lens`) to the emitting atom (D3).
fn trace_camera(level: &Level, scene_node: &EffectGraphNode) -> CameraVm {
    let Some((mut node_id, _)) = level.producer(scene_node.id, "camera") else {
        return CameraVm::None;
    };
    let mut lens_node_doc_id = None;
    // At most one pass-through hop is the shipped shape (importer's lens);
    // walk generically in case a future graph chains more than one.
    let mut guard = 0;
    loop {
        guard += 1;
        if guard > 8 {
            break;
        }
        let Some(node) = level.node(node_id) else { return CameraVm::None };
        if node.type_id == CAMERA_LENS_TYPE_ID {
            lens_node_doc_id = Some(node.id);
            match level.producer(node.id, "camera") {
                Some((next, _)) => {
                    node_id = next;
                    continue;
                }
                None => return CameraVm::Custom { node_doc_id: node.id },
            }
        }
        break;
    }
    let Some(node) = level.node(node_id) else { return CameraVm::None };
    let lens = lens_node_doc_id.and_then(|id| trace_lens(level, id));
    match node.type_id.as_str() {
        t if t == ORBIT_CAMERA_TYPE_ID => {
            CameraVm::Orbit(Box::new(OrbitCameraRow { node_doc_id: node.id, lens }))
        }
        t if t == FREE_CAMERA_TYPE_ID => {
            CameraVm::Free(Box::new(FreeCameraRow { node_doc_id: node.id, lens }))
        }
        t if t == LOOK_AT_CAMERA_TYPE_ID => {
            CameraVm::LookAt(Box::new(LookAtCameraRow { node_doc_id: node.id, lens }))
        }
        _ => CameraVm::Custom { node_doc_id: node.id },
    }
}

fn trace_environment(level: &Level, scene_node: &EffectGraphNode) -> EnvironmentVm {
    let Some((node_id, _)) = level.producer(scene_node.id, "envmap") else {
        return EnvironmentVm::None;
    };
    let Some(node) = level.node(node_id) else { return EnvironmentVm::None };

    if node.type_id == SWITCH_TEXTURE_TYPE_ID {
        // Importer shape: in_0 = bake_environment, in_1 = exposure(hdri_source).
        let bake = level
            .producer(node.id, "in_0")
            .and_then(|(n, _)| level.node(n))
            .filter(|n| n.type_id == BAKE_ENVIRONMENT_TYPE_ID);
        let hdri_chain = level.producer(node.id, "in_1").and_then(|(gain_id, _)| {
            let gain_node = level.node(gain_id).filter(|n| n.type_id == EXPOSURE_TYPE_ID)?;
            let (hdri_id, _) = level.producer(gain_node.id, "in")?;
            let hdri_node = level.node(hdri_id).filter(|n| n.type_id == HDRI_SOURCE_TYPE_ID)?;
            Some(hdri_node.id)
        });
        if let (Some(bake), Some(hdri_id)) = (bake, hdri_chain) {
            let hdri_file_value = level
                .node(hdri_id)
                .and_then(|n| n.params.get("path"))
                .and_then(|v| match v {
                    SerializedParamValue::String { value } => Some(value.clone()),
                    _ => None,
                })
                .unwrap_or_default();
            return EnvironmentVm::Importer(Box::new(ImporterEnvironmentRow {
                switch_node_id: node.id,
                bake_node_id: bake.id,
                hdri_node_id: hdri_id,
                hdri_file_value,
            }));
        }
        return EnvironmentVm::Custom { node_doc_id: node.id };
    }

    if node.type_id == BAKE_ENVIRONMENT_TYPE_ID {
        return EnvironmentVm::Bare(Box::new(BareEnvironmentRow { node_doc_id: node.id }));
    }

    EnvironmentVm::Custom { node_doc_id: node.id }
}

fn trace_atmosphere(level: &Level, scene_node: &EffectGraphNode) -> AtmosphereVm {
    let Some((node_id, _)) = level.producer(scene_node.id, "atmosphere") else {
        return AtmosphereVm::None;
    };
    let Some(node) = level.node(node_id) else { return AtmosphereVm::None };
    if node.type_id != ATMOSPHERE_TYPE_ID {
        // Some other producer wired into `atmosphere` — D3 has no "custom
        // atmosphere row" concept distinct from None; treat as unwired-shape
        // (Add fog would create a second, redundant atmosphere node only if
        // the panel doesn't check first — the panel checks `AtmosphereVm`
        // before offering the add action, so this never double-adds).
        return AtmosphereVm::None;
    }
    AtmosphereVm::Wired(Box::new(AtmosphereRow { node_doc_id: node.id }))
}

/// UX-P3a (SCENE_PANEL_UX_DESIGN.md D8/sizing amendment): whether `param_id`
/// is currently exposed on the outer card for the node with doc id
/// `node_doc_id`, searched at any depth (root or inside a group body).
/// `exposed_params` is a per-node `BTreeSet<String>` already on
/// [`EffectGraphNode`] — this is the "free read" the amendment names, a
/// second independent walk of the SAME `def` `SceneVm::from_def` just
/// walked (node doc ids are unique document-wide, so no scope disambiguation
/// is needed). The panel rebuilds this on every event-gated sync anyway
/// (D1: "no rotting, no staleness"), so a second O(nodes) pass costs
/// nothing measurable — no lookup table plumbed through the Vm tree.
pub fn is_param_exposed(def: &EffectGraphDef, node_doc_id: u32, param_id: &str) -> bool {
    fn search(nodes: &[EffectGraphNode], node_doc_id: u32, param_id: &str) -> Option<bool> {
        for n in nodes {
            if n.id == node_doc_id {
                return Some(n.exposed_params.contains(param_id));
            }
            if let Some(group) = &n.group
                && let Some(found) = search(&group.nodes, node_doc_id, param_id)
            {
                return Some(found);
            }
        }
        None
    }
    search(&def.nodes, node_doc_id, param_id).unwrap_or(false)
}

/// Sibling of [`is_param_exposed`]: true when `(node_doc_id, param_id)` has a
/// wire feeding it at the level the node lives on — i.e. the param is
/// wire-driven and renders read-only (wire-wins-at-eval). Replaces the
/// per-struct `_driven` fields the scene VMs used to transcribe; the panel's
/// manifest rows now source driven-state through here. Mirrors the
/// `level.producer(node_id, name).is_some()` checks `from_def` used internally.
pub fn is_param_driven(def: &EffectGraphDef, node_doc_id: u32, param_id: &str) -> bool {
    fn search(
        nodes: &[EffectGraphNode],
        wires: &[manifold_core::effect_graph_def::EffectGraphWire],
        node_doc_id: u32,
        param_id: &str,
    ) -> Option<bool> {
        for n in nodes {
            if n.id == node_doc_id {
                return Some(wires.iter().any(|w| w.to_node == node_doc_id && w.to_port == param_id));
            }
            if let Some(group) = &n.group
                && let Some(found) = search(&group.nodes, &group.wires, node_doc_id, param_id)
            {
                return Some(found);
            }
        }
        None
    }
    search(&def.nodes, &def.wires, node_doc_id, param_id).unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use manifold_core::effect_graph_def::{EffectGraphWire, GroupDef, GroupInterface};
    use std::collections::BTreeMap;

    fn node(id: u32, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: Default::default(),
            type_id: type_id.to_string(),
            handle: handle.map(|s| s.to_string()),
            params: BTreeMap::new(),
            exposed_params: Default::default(),
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

    fn with_param(mut n: EffectGraphNode, k: &str, v: SerializedParamValue) -> EffectGraphNode {
        n.params.insert(k.to_string(), v);
        n
    }

    fn def(nodes: Vec<EffectGraphNode>, wires: Vec<EffectGraphWire>) -> EffectGraphDef {
        EffectGraphDef {
            version: 1,
            name: None,
            description: None,
            preset_metadata: None,
            nodes,
            wires,
        }
    }

    #[test]
    fn empty_def_yields_no_scene() {
        let d = def(vec![node(0, "system.final_output", None)], vec![]);
        assert!(SceneVm::from_def(&d).is_none());
    }

    #[test]
    fn def_with_no_final_output_yields_no_scene() {
        let d = def(vec![node(0, RENDER_SCENE_TYPE_ID, None)], vec![]);
        assert!(SceneVm::from_def(&d).is_none());
    }

    #[test]
    fn unreachable_render_scene_is_not_the_root() {
        // A render_scene that doesn't wire to the output must not be picked.
        let d = def(
            vec![
                node(0, RENDER_SCENE_TYPE_ID, None), // orphaned
                node(1, "system.final_output", None),
            ],
            vec![],
        );
        assert!(SceneVm::from_def(&d).is_none());
    }

    #[test]
    fn two_render_scenes_picks_first_by_id_and_flags_multiple() {
        let d = def(
            vec![
                node(5, RENDER_SCENE_TYPE_ID, None),
                node(2, RENDER_SCENE_TYPE_ID, None),
                node(9, "system.final_output", None),
            ],
            vec![wire(2, "color", 9, "in")],
        );
        // Only node 2 is reachable (wired to output); node 5 is orphaned —
        // so this exercises "not reachable" rather than the tie-break. Add
        // a second reachable one explicitly:
        let d2 = def(
            vec![
                node(5, RENDER_SCENE_TYPE_ID, None),
                node(2, RENDER_SCENE_TYPE_ID, None),
                node(6, "node.value", None), // pass-through stand-in
                node(9, "system.final_output", None),
            ],
            vec![
                wire(2, "color", 9, "in"),
                wire(5, "color", 6, "in"),
                wire(6, "out", 9, "in"),
            ],
        );
        let vm = SceneVm::from_def(&d2).unwrap();
        assert_eq!(vm.scene_root_node_id, 2);
        assert!(vm.multiple_scenes);

        let vm1 = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm1.scene_root_node_id, 2);
        assert!(!vm1.multiple_scenes);
    }

    fn importer_shaped_def() -> EffectGraphDef {
        let scene = with_param(
            with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 0.0 }),
            "lights",
            SerializedParamValue::Float { value: 1.0 },
        );
        let cam = node(1, ORBIT_CAMERA_TYPE_ID, Some("camera"));
        let lens = node(2, CAMERA_LENS_TYPE_ID, Some("lens"));
        let envmap = node(3, BAKE_ENVIRONMENT_TYPE_ID, Some("envmap"));
        let hdri = node(4, HDRI_SOURCE_TYPE_ID, Some("hdri"));
        let gain = node(5, EXPOSURE_TYPE_ID, Some("hdri_gain"));
        let select = node(6, SWITCH_TEXTURE_TYPE_ID, Some("env_select"));
        let atmo = node(7, ATMOSPHERE_TYPE_ID, Some("atmosphere"));
        let sun = with_param(node(8, LIGHT_TYPE_ID, Some("sun")), "cast_shadows", SerializedParamValue::Float { value: 1.0 });
        let out = node(20, "system.final_output", None);
        def(
            vec![cam, lens, envmap, hdri, gain, select, atmo, sun, scene, out],
            vec![
                wire(1, "out", 2, "camera"),
                wire(2, "out", 10, "camera"),
                wire(3, "envmap", 6, "in_0"),
                wire(4, "out", 5, "in"),
                wire(5, "out", 6, "in_1"),
                wire(6, "out", 10, "envmap"),
                wire(7, "atmosphere", 10, "atmosphere"),
                wire(8, "out", 10, "light_0"),
                wire(10, "color", 20, "in"),
            ],
        )
    }

    #[test]
    fn importer_shaped_environment_and_camera_trace() {
        let d = importer_shaped_def();
        let vm = SceneVm::from_def(&d).unwrap();
        match vm.environment {
            EnvironmentVm::Importer(row) => {
                assert_eq!(row.hdri_node_id, 4);
            }
            other => panic!("expected Importer shape, got {other:?}"),
        }
        match vm.camera {
            CameraVm::Orbit(row) => {
                assert_eq!(row.node_doc_id, 1);
                assert_eq!(row.lens.as_ref().map(|l| l.node_doc_id), Some(2));
            }
            other => panic!("expected Orbit camera, got {other:?}"),
        }
        match vm.atmosphere {
            AtmosphereVm::Wired(row) => assert_eq!(row.node_doc_id, 7),
            other => panic!("expected Wired atmosphere, got {other:?}"),
        }
        assert_eq!(vm.lights.len(), 1);
        match &vm.lights[0] {
            SceneLightVm::Known(row) => assert_eq!(row.node_doc_id, 8),
            other => panic!("expected Known light, got {other:?}"),
        }
        assert_eq!(vm.header.shadow_caster_count, 1);
    }

    #[test]
    fn bare_bake_environment_and_unwired_fog() {
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 0.0 });
        let envmap = node(3, BAKE_ENVIRONMENT_TYPE_ID, Some("envmap"));
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![envmap, scene, out],
            vec![wire(3, "envmap", 10, "envmap"), wire(10, "color", 20, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert!(matches!(vm.environment, EnvironmentVm::Bare { .. }));
        assert!(matches!(vm.atmosphere, AtmosphereVm::None));
        assert!(matches!(vm.camera, CameraVm::None));
    }

    #[test]
    fn hand_built_object_wired_directly_to_a_mesh_source_degrades_to_custom() {
        // A render_scene wired directly to a bare mesh source (no
        // scene_object at all) — the D3/D12 "Object k — custom (edit in
        // graph)" case.
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![mesh, scene, out],
            vec![wire(1, "vertices", 10, "object_0"), wire(10, "color", 20, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.objects.len(), 1);
        assert!(matches!(vm.objects[0], SceneObjectVm::Custom { index: 0 }));
    }

    /// D1/D12: a bare `node.scene_object` wired DIRECTLY to `object_0` — no
    /// wrapping group at all — must still resolve as a first-class `Known`
    /// row, scoped at root (empty `scope_path`, `group_node_id: None`).
    #[test]
    fn ungrouped_scene_object_resolves_known_at_root_scope() {
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let mat = with_param(node(2, "node.phong_material", Some("mat")), "color_r", SerializedParamValue::Float { value: 0.4 });
        let transform = with_param(node(3, TRANSFORM_3D_TYPE_ID, Some("t")), "pos_x", SerializedParamValue::Float { value: 4.0 });
        let obj = node(4, SCENE_OBJECT_TYPE_ID, Some("Bare Hero"));
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![mesh, mat, transform, obj, scene, out],
            vec![
                wire(1, "vertices", 4, "vertices"),
                wire(2, "out", 4, "material"),
                wire(3, "transform", 4, "transform"),
                wire(4, "object", 10, "object_0"),
                wire(10, "color", 20, "in"),
            ],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.objects.len(), 1);
        match &vm.objects[0] {
            SceneObjectVm::Known(row) => {
                assert_eq!(row.object_node_id, 4);
                assert_eq!(row.group_node_id, None, "no wrapping group — root scope");
                assert_eq!(row.name, "Bare Hero");
                assert!(row.visible_value, "visible param defaults on");
                assert!(row.modifier_chain.is_empty());
                assert!(row.modifier_chain_parseable);
                let t = row.transform.as_ref().expect("transform input resolves");
                assert_eq!(t.node_doc_id, 3);
                assert_eq!(t.pos_value.0, 4.0);
                assert!(t.pos_addr.0.scope_path.is_empty(), "root-level transform has an empty scope");
                match &row.material {
                    MaterialVm::Known(m) => assert!(!m.is_pbr, "phong material is not PBR"),
                    MaterialVm::None => panic!("expected a resolved material"),
                }
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    /// The importer/`AddSceneObjectCommand` shape: mesh/material/transform +
    /// `node.scene_object` all live INSIDE a group; the group's own `object`
    /// interface output re-exports the scene_object's `object` port to the
    /// root `object_k` wire.
    fn grouped_scene_object_def(object_id: u32, group_id: u32, port_index: usize, name: &str) -> (EffectGraphNode, Vec<EffectGraphWire>) {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(object_id + 100, "node.cube_mesh", Some("mesh"));
        let bend = node(object_id + 101, "node.bend_mesh", Some("bend"));
        let mat = with_param(
            node(object_id + 102, "node.phong_material", Some("mat")),
            "color_r",
            SerializedParamValue::Float { value: 0.4 },
        );
        let transform = with_param(
            node(object_id + 103, TRANSFORM_3D_TYPE_ID, Some("transform")),
            "pos_y",
            SerializedParamValue::Float { value: 2.5 },
        );
        let scene_obj = node(object_id, SCENE_OBJECT_TYPE_ID, Some(name));
        let gout = node(object_id + 104, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(group_id, GROUP_TYPE_ID, Some(name));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, bend, mat, transform, scene_obj, gout],
            wires: vec![
                wire(object_id + 100, "vertices", object_id + 101, "in"),
                wire(object_id + 101, "out", object_id, "vertices"),
                wire(object_id + 102, "out", object_id, "material"),
                wire(object_id + 103, "transform", object_id, "transform"),
                wire(object_id, "object", object_id + 104, "object"),
            ],
            tint: Some([0.1, 0.2, 0.3, 1.0]),
        }));
        (group_node, vec![wire(group_id, "object", 10, &format!("object_{port_index}"))])
    }

    #[test]
    fn grouped_scene_object_resolves_with_modifier_chain_material_and_transform() {
        let (group_node, top_wires) = grouped_scene_object_def(1, 2, 0, "Hero");
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(20, "system.final_output", None);
        let mut wires = top_wires;
        wires.push(wire(10, "color", 20, "in"));
        let d = def(vec![group_node, scene, out], wires);
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.objects.len(), 1);
        match &vm.objects[0] {
            SceneObjectVm::Known(row) => {
                assert_eq!(row.object_node_id, 1);
                assert_eq!(row.group_node_id, Some(2));
                assert_eq!(row.name, "Hero");
                assert_eq!(row.modifier_chain.len(), 1);
                assert_eq!(row.modifier_chain[0].type_id, "node.bend_mesh");
                assert!(row.modifier_chain_parseable, "a well-formed one-modifier chain parses");
                match &row.material {
                    MaterialVm::Known(m) => {
                        assert!(!m.is_pbr, "phong material is not PBR");
                        assert_eq!(m.scope_path, vec![2], "material lives inside the group — scoped address");
                    }
                    MaterialVm::None => panic!("expected a resolved material"),
                }
                let t = row.transform.as_ref().expect("transform resolves through the scene_object's transform input");
                assert_eq!(t.node_doc_id, 104);
                assert_eq!(t.pos_value.1, 2.5);
                assert_eq!(t.pos_addr.1.scope_path, vec![2], "transform lives inside the group — scoped address");
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    /// Mixed scene: one grouped object (the importer shape) and one bare
    /// ungrouped `node.scene_object`, in the same def — both resolve `Known`.
    #[test]
    fn mixed_grouped_and_ungrouped_objects_both_resolve_known() {
        let (group_node, group_wires) = grouped_scene_object_def(1, 2, 0, "Grouped");
        let bare_mesh = node(50, "node.cube_mesh", Some("mesh"));
        let bare_obj = node(51, SCENE_OBJECT_TYPE_ID, Some("Bare"));
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 2.0 });
        let out = node(20, "system.final_output", None);
        let mut wires = group_wires;
        wires.push(wire(50, "vertices", 51, "vertices"));
        wires.push(wire(51, "object", 10, "object_1"));
        wires.push(wire(10, "color", 20, "in"));
        let d = def(vec![group_node, bare_mesh, bare_obj, scene, out], wires);
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.objects.len(), 2);
        match &vm.objects[0] {
            SceneObjectVm::Known(row) if row.group_node_id == Some(2) => assert_eq!(row.name, "Grouped"),
            other => panic!("expected grouped Known object at index 0, got {other:?}"),
        }
        match &vm.objects[1] {
            SceneObjectVm::Known(row) if row.group_node_id.is_none() => assert_eq!(row.name, "Bare"),
            other => panic!("expected ungrouped Known object at index 1, got {other:?}"),
        }
    }

    /// D12: something other than `node.scene_object` (directly, or through a
    /// group) feeding `object_k` degrades to `Custom` — never hidden, never
    /// an error.
    #[test]
    fn custom_producer_that_isnt_scene_object_shaped_degrades_to_custom() {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        // A group whose output doesn't come from a scene_object at all.
        let value_node = node(1, "node.value", Some("weird"));
        let gout = node(2, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(3, GROUP_TYPE_ID, Some("NotAnObject"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![value_node, gout],
            wires: vec![wire(1, "out", 2, "object")],
            tint: None,
        }));
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(3, "object", 10, "object_0"), wire(10, "color", 20, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.objects.len(), 1);
        assert!(matches!(vm.objects[0], SceneObjectVm::Custom { index: 0 }));
    }

    #[test]
    fn modifier_chain_captures_identity() {
        // P5: the chain walk captures each modifier's identity only
        // (node_doc_id/type_id) — `state_sync` reads each modifier's own
        // params/driven-state generically off the def via `is_param_driven`
        // and the manifest closures, keyed on that node id.
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let angle_driver = node(2, "node.value", Some("angle_driver"));
        let bend = node(3, "node.bend_mesh", Some("bend"));
        let scene_obj = node(4, SCENE_OBJECT_TYPE_ID, Some("Obj"));
        let gout = node(5, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Obj"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, angle_driver, bend, scene_obj, gout],
            wires: vec![
                wire(1, "vertices", 3, "in"),
                wire(2, "out", 3, "angle"), // port-shadow: angle is wired, driven
                wire(3, "out", 4, "vertices"),
                wire(4, "object", 5, "object"),
            ],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match &vm.objects[0] {
            SceneObjectVm::Known(row) => {
                assert!(row.modifier_chain_parseable);
                assert_eq!(row.modifier_chain.len(), 1);
                let m = &row.modifier_chain[0];
                assert_eq!(m.node_doc_id, 3);
                assert_eq!(m.type_id, "node.bend_mesh");
                assert!(is_param_driven(&d, 3, "angle"), "angle is wired — driven, via the shared helper");
                assert!(!is_param_driven(&d, 3, "center"), "center is a plain param, not wired");
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    #[test]
    fn zero_modifiers_with_mesh_source_feeding_scene_object_directly_is_still_parseable() {
        // A fresh object (no modifiers yet): `vertices` resolves straight to
        // the mesh source. `modifier_chain` is empty but `parseable` is
        // still `true` — distinct from the genuinely-unparseable case below.
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let scene_obj = node(2, SCENE_OBJECT_TYPE_ID, Some("Obj"));
        let gout = node(3, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Obj"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, scene_obj, gout],
            wires: vec![wire(1, "vertices", 2, "vertices"), wire(2, "object", 3, "object")],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match &vm.objects[0] {
            SceneObjectVm::Known(row) => {
                assert!(row.modifier_chain.is_empty());
                assert!(row.modifier_chain_parseable, "zero modifiers is a valid, addable stack");
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    #[test]
    fn unwired_vertices_port_is_unparseable_custom_chain() {
        // D6: a scene_object whose `vertices` port is unwired entirely — the
        // panel must show "custom chain — edit in graph" and disable Add,
        // never guess at a splice point.
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let scene_obj = node(1, SCENE_OBJECT_TYPE_ID, Some("Obj"));
        let gout = node(2, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Obj"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![scene_obj, gout],
            wires: vec![wire(1, "object", 2, "object")],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match &vm.objects[0] {
            SceneObjectVm::Known(row) => {
                assert!(row.modifier_chain.is_empty());
                assert!(!row.modifier_chain_parseable, "unwired vertices port is unparseable");
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    #[test]
    fn pbr_material_gets_metallic_roughness_but_phong_does_not() {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let mat = with_param(
            with_param(node(2, "node.pbr_material", Some("mat")), "metallic", SerializedParamValue::Float { value: 0.7 }),
            "roughness",
            SerializedParamValue::Float { value: 0.3 },
        );
        let scene_obj = node(3, SCENE_OBJECT_TYPE_ID, Some("Pbr"));
        let gout = node(4, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Pbr"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, mat, scene_obj, gout],
            wires: vec![
                wire(1, "vertices", 3, "vertices"),
                wire(2, "out", 3, "material"),
                wire(3, "object", 4, "object"),
            ],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match &vm.objects[0] {
            SceneObjectVm::Known(row) if matches!(row.material, MaterialVm::Known(_)) => {
                let MaterialVm::Known(m) = &row.material else { unreachable!() };
                assert!(m.is_pbr, "pbr material atom flags is_pbr — metallic/roughness rows follow");
            }
            other => panic!("expected Known pbr object, got {other:?}"),
        }
    }

    /// D12's `visible` port-shadow: a wire into `visible` reads as driven,
    /// with the current threshold value still resolved.
    #[test]
    fn visible_port_shadow_reads_value_and_driven_flag() {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let lfo = node(2, "node.value", Some("mute_lfo"));
        let scene_obj = with_param(node(3, SCENE_OBJECT_TYPE_ID, Some("Obj")), "visible", SerializedParamValue::Float { value: 0.0 });
        let gout = node(4, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Obj"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, lfo, scene_obj, gout],
            wires: vec![
                wire(1, "vertices", 3, "vertices"),
                wire(2, "out", 3, "visible"),
                wire(3, "object", 4, "object"),
            ],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match &vm.objects[0] {
            SceneObjectVm::Known(row) => {
                assert!(!row.visible_value, "the stored param value reads 0.0 = off");
                assert!(row.visible_driven, "a wire feeds visible — driven");
                assert_eq!(row.visible_addr.param_id, "visible");
                assert_eq!(row.visible_addr.scope_path, vec![10]);
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    #[test]
    fn scene_vm_is_pure_no_project_type_referenced() {
        // Compile-time proof by construction: this module imports nothing
        // from `manifold_core::project`. The negative `rg` gate (§4) checks
        // the same claim textually across the file.
        let d = importer_shaped_def();
        let vm1 = SceneVm::from_def(&d);
        let vm2 = SceneVm::from_def(&d);
        assert_eq!(vm1, vm2, "from_def must be a pure function of the def alone");
    }

    // ── P3: Lights + Camera sections ──

    #[test]
    fn known_light_row_resolves_identity() {
        let light = with_param(
            node(8, LIGHT_TYPE_ID, Some("sun")),
            "cast_shadows",
            SerializedParamValue::Float { value: 1.0 },
        );
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "lights", SerializedParamValue::Float { value: 1.0 });
        let out = node(20, "system.final_output", None);
        let d = def(vec![light, scene, out], vec![wire(8, "out", 10, "light_0"), wire(10, "color", 20, "in")]);
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.lights.len(), 1);
        match &vm.lights[0] {
            SceneLightVm::Known(row) => {
                assert_eq!(row.node_doc_id, 8);
                assert_eq!(row.name, "sun");
            }
            other => panic!("expected Known light, got {other:?}"),
        }
        assert_eq!(vm.header.shadow_caster_count, 1);
    }

    #[test]
    fn more_than_four_shadow_casters_all_resolve_no_panel_side_cap() {
        // REALTIME_3D D4's K=4 shadow-caster cap is the RENDERER's job; the
        // Vm/panel must never enforce or truncate it — a scene with 5
        // casters still traces (and the panel would render) every light row.
        let mut nodes = Vec::new();
        let mut wires = Vec::new();
        for i in 0..5u32 {
            let id = 100 + i;
            nodes.push(with_param(
                node(id, LIGHT_TYPE_ID, Some(&format!("light{i}"))),
                "cast_shadows",
                SerializedParamValue::Float { value: 1.0 },
            ));
            wires.push(wire(id, "out", 10, &format!("light_{i}")));
        }
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "lights", SerializedParamValue::Float { value: 5.0 });
        nodes.push(scene);
        nodes.push(node(20, "system.final_output", None));
        wires.push(wire(10, "color", 20, "in"));
        let d = def(nodes, wires);
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.lights.len(), 5, "all 5 lights resolve, no cap in the Vm");
        assert_eq!(vm.header.shadow_caster_count, 5, "the header reports the true count, uncapped");
        assert!(vm.lights.iter().all(|l| matches!(l, SceneLightVm::Known(_))));
    }

    #[test]
    fn free_camera_with_lens_pass_through_traces_identity() {
        let cam = node(1, FREE_CAMERA_TYPE_ID, Some("camera"));
        let lens = node(2, CAMERA_LENS_TYPE_ID, Some("lens"));
        let scene = node(10, RENDER_SCENE_TYPE_ID, None);
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![cam, lens, scene, out],
            vec![wire(1, "out", 2, "camera"), wire(2, "out", 10, "camera"), wire(10, "color", 20, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match vm.camera {
            CameraVm::Free(row) => {
                assert_eq!(row.node_doc_id, 1);
                let lens_row = row.lens.as_ref().expect("lens pass-through resolves");
                assert_eq!(lens_row.node_doc_id, 2);
            }
            other => panic!("expected Free camera, got {other:?}"),
        }
    }

    #[test]
    fn look_at_camera_shape_traces_identity() {
        let cam = node(1, LOOK_AT_CAMERA_TYPE_ID, Some("camera"));
        let scene = node(10, RENDER_SCENE_TYPE_ID, None);
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![cam, scene, out],
            vec![wire(1, "out", 10, "camera"), wire(10, "color", 20, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match vm.camera {
            CameraVm::LookAt(row) => {
                assert_eq!(row.node_doc_id, 1);
                assert!(row.lens.is_none(), "no lens node wired — no pass-through to trace");
            }
            other => panic!("expected LookAt camera, got {other:?}"),
        }
    }

    #[test]
    fn camera_producer_that_isnt_a_curated_atom_degrades_to_custom() {
        let cam = node(1, "node.value", Some("weird"));
        let scene = node(10, RENDER_SCENE_TYPE_ID, None);
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![cam, scene, out],
            vec![wire(1, "out", 10, "camera"), wire(10, "color", 20, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert!(matches!(vm.camera, CameraVm::Custom { node_doc_id: 1 }));
    }

    /// BUG-194: a `node.gltf_mesh_source` with a known `source_vertex_count`
    /// feeds the header's vertex-count row exactly, with `vertex_count_exact
    /// == true` — the honest, non-fabricated case.
    #[test]
    fn header_vertex_count_sums_known_gltf_mesh_source() {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = with_param(
            node(1, "node.gltf_mesh_source", Some("mesh")),
            "source_vertex_count",
            SerializedParamValue::Int { value: 1234 },
        );
        let scene_obj = node(2, SCENE_OBJECT_TYPE_ID, Some("Obj"));
        let gout = node(3, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Obj"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, scene_obj, gout],
            wires: vec![wire(1, "vertices", 2, "vertices"), wire(2, "object", 3, "object")],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.header.vertex_count, 1234);
        assert!(vm.header.vertex_count_exact, "a known source_vertex_count must report exact, not ≥");
    }

    /// BUG-194: `source_vertex_count` still at its `-1` "unknown" default
    /// (a hand-built node the importer never touched) degrades the header
    /// to a lower bound — `vertex_count_exact == false` — never a
    /// fabricated 0 or a silently-omitted object.
    #[test]
    fn header_vertex_count_degrades_to_lower_bound_when_unknown() {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        // No source_vertex_count param at all == the primitive's own -1
        // "unknown" default (never read, so absent-from-map behaves the
        // same as an explicit -1).
        let mesh = node(1, "node.gltf_mesh_source", Some("mesh"));
        let scene_obj = node(2, SCENE_OBJECT_TYPE_ID, Some("Obj"));
        let gout = node(3, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Obj"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, scene_obj, gout],
            wires: vec![wire(1, "vertices", 2, "vertices"), wire(2, "object", 3, "object")],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.header.vertex_count, 0, "no known contribution — sum stays 0, never fabricated");
        assert!(!vm.header.vertex_count_exact, "an unresolved mesh source must degrade to ≥, not report 0 as exact");
    }

    /// BUG-194's closed-form table: `node.cube_mesh` (fixed 36) and
    /// `node.grid_mesh` (`resolution_x * resolution_y`) contribute exact
    /// counts with no import-time provenance needed at all.
    #[test]
    fn header_vertex_count_covers_procedural_closed_form_generators() {
        let group_iface = || GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };

        let cube_scene_obj = node(2, SCENE_OBJECT_TYPE_ID, Some("Cube"));
        let cube_gout = node(3, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut cube_group = node(10, GROUP_TYPE_ID, Some("Cube"));
        cube_group.group = Some(Box::new(GroupDef {
            interface: group_iface(),
            nodes: vec![node(1, "node.cube_mesh", Some("mesh")), cube_scene_obj, cube_gout],
            wires: vec![wire(1, "vertices", 2, "vertices"), wire(2, "object", 3, "object")],
            tint: None,
        }));

        let grid = with_param(
            with_param(
                node(4, "node.grid_mesh", Some("mesh")),
                "resolution_x",
                SerializedParamValue::Int { value: 8 },
            ),
            "resolution_y",
            SerializedParamValue::Int { value: 4 },
        );
        let grid_scene_obj = node(5, SCENE_OBJECT_TYPE_ID, Some("Grid"));
        let grid_gout = node(6, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut grid_group = node(11, GROUP_TYPE_ID, Some("Grid"));
        grid_group.group = Some(Box::new(GroupDef {
            interface: group_iface(),
            nodes: vec![grid, grid_scene_obj, grid_gout],
            wires: vec![wire(4, "vertices", 5, "vertices"), wire(5, "object", 6, "object")],
            tint: None,
        }));

        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 2.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![cube_group, grid_group, scene, out],
            vec![
                wire(10, "object", 20, "object_0"),
                wire(11, "object", 20, "object_1"),
                wire(20, "color", 30, "in"),
            ],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.header.vertex_count, 36 + 8 * 4);
        assert!(vm.header.vertex_count_exact);
    }

    /// Regression gate for the migrated-project shape `migrate_scene_object_wires`
    /// actually produces (D5's "same-scope re-point"): a minted `node.scene_object`
    /// stays a ROOT-level sibling of the mesh producer's group, `vertices`/
    /// `material`/`transform` wired straight from the GROUP's own boundary port —
    /// not nested inside it (the shape a fresh glTF import produces instead,
    /// already covered by this file's `grouped_scene_object_def`-style tests).
    /// without `resolve_producer_through_group`,
    /// every already-shipped, already-migrated bundled preset (SceneStarter and
    /// the ~9 others P2 regenerated) silently showed no transform/material
    /// controls and a wrong vertex count in the panel, despite rendering
    /// correctly (the render path reads through `SceneObject`'s resolved Slots,
    /// never through this trace).
    #[test]
    fn bundled_scene_starter_preset_resolves_transform_material_and_vertex_count() {
        let preset_type = manifold_core::PresetTypeId::from_string("SceneStarter".to_string());
        let d = crate::node_graph::bundled_presets::bundled_preset_def(&preset_type)
            .expect("SceneStarter is a bundled preset");
        let vm = SceneVm::from_def(d).expect("SceneStarter resolves");
        assert_eq!(vm.objects.len(), 2, "Floor + Cube");
        for obj in &vm.objects {
            let SceneObjectVm::Known(row) = obj else {
                panic!("SceneStarter's objects must resolve Known, not Custom — migration shape unparsed");
            };
            assert!(row.transform.is_some(), "{}: transform must resolve through the group boundary", row.name);
            assert!(
                !matches!(row.material, MaterialVm::None),
                "{}: material must resolve through the group boundary",
                row.name
            );
        }
        assert!(vm.header.vertex_count > 0, "vertex count must resolve through the group boundary, not silently 0");
        assert!(vm.header.vertex_count_exact, "SceneStarter's mesh sources have known vertex counts");
    }

    /// UX-P3a's exposed-state read (D8): unexposed by default, flips on
    /// `exposed_params` insert, and finds a node nested inside a group body
    /// (the scene_object/transform_3d shape D12 wraps grouped objects in).
    #[test]
    fn is_param_exposed_reads_root_and_grouped_nodes() {
        let mut root_node = node(1, TRANSFORM_3D_TYPE_ID, None);
        assert!(!is_param_exposed(&def(vec![root_node.clone()], vec![]), 1, "pos_x"));
        root_node.exposed_params.insert("pos_x".to_string());
        let d = def(vec![root_node], vec![]);
        assert!(is_param_exposed(&d, 1, "pos_x"));
        assert!(!is_param_exposed(&d, 1, "pos_y"), "only the inserted param is exposed");
        assert!(!is_param_exposed(&d, 99, "pos_x"), "unknown node id — no panic, just false");

        let mut inner = node(10, MATERIAL_TYPE_IDS[0], None);
        inner.exposed_params.insert("roughness".to_string());
        let mut group = node(11, GROUP_TYPE_ID, Some("Cube"));
        group.group = Some(Box::new(GroupDef {
            interface: GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
            nodes: vec![inner],
            wires: vec![],
            tint: None,
        }));
        let grouped_def = def(vec![group], vec![]);
        assert!(is_param_exposed(&grouped_def, 10, "roughness"), "must find nodes nested inside a group body");
    }

    #[test]
    fn is_param_driven_reads_root_and_grouped_wires() {
        let root_node = node(1, TRANSFORM_3D_TYPE_ID, None);
        let source = node(0, "node.other", None);
        let d = def(
            vec![source.clone(), root_node.clone()],
            vec![wire(0, "out", 1, "pos_x")],
        );
        assert!(is_param_driven(&d, 1, "pos_x"), "wired param at root must be driven");
        assert!(!is_param_driven(&d, 1, "pos_y"), "unwired param at root must not be driven");
        assert!(!is_param_driven(&d, 99, "pos_x"), "unknown node id — no panic, just false");

        let inner = node(10, MATERIAL_TYPE_IDS[0], None);
        let inner_source = node(20, "node.other", None);
        let mut group = node(11, GROUP_TYPE_ID, Some("Cube"));
        group.group = Some(Box::new(GroupDef {
            interface: GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
            nodes: vec![inner_source, inner],
            wires: vec![wire(20, "out", 10, "roughness")],
            tint: None,
        }));
        let grouped_def = def(vec![group], vec![]);
        assert!(is_param_driven(&grouped_def, 10, "roughness"), "must find wires at the level inside a group body");
        assert!(!is_param_driven(&grouped_def, 10, "metallic"), "unwired grouped param must not be driven");
    }
}
