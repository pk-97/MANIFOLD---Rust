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
//! Which exposed controls belong to a row is a separate question from where
//! its values are written: every row also carries the stable `NodeId` of
//! each node it owns, and a control belongs to the row whose node its
//! primary binding targets. Document ids are only unique inside one graph
//! level, so they never decide ownership.
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

use std::collections::{HashMap, HashSet};

use manifold_core::liquid_domain::{is_liquid_domain, liquid_domain_of};
use manifold_core::scene_index::FlatSceneIndex;
use manifold_core::{LayerId, NodeId, SceneNodeRef};
use manifold_core::effect_graph_def::{
    EffectGraphDef, EffectGraphNode, GROUP_OUTPUT_TYPE_ID, GROUP_TYPE_ID, SerializedParamValue,
};

use crate::node_graph::FINAL_OUTPUT_TYPE_ID;
use crate::node_graph::fluid::{FluidDomainLayout, FluidSettings};
use crate::node_graph::transform::Transform;

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
const LOOP_CAMERA_TYPE_ID: &str = "node.loop_camera";
const CAMERA_LENS_TYPE_ID: &str = "node.camera_lens";
const MOTION_BLUR_TYPE_ID: &str = "node.motion_blur";
const BOKEH_GATHER_TYPE_ID: &str = "node.bokeh_gather";
/// PBR/unlit/cel — the three material atoms (D3's Objects material row).
const MATERIAL_TYPE_IDS: &[&str] = &[
    "node.pbr_material",
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
    "node.voxelize_mesh",
    "node.noise_displace",
    "node.glitch_jitter",
    "node.shatter_mesh",
    "node.slice_mesh",
    "node.ripple_mesh",
    "node.fold_mesh",
    "node.melt_mesh",
    "node.wave_shear_mesh",
    "node.transform_mesh_patches",
];
/// The curated Transform-chain modifier vocabulary (P3): single-Transform-in/
/// Transform-out atoms that may sit between `node.transform_3d` and
/// `node.scene_object`'s `transform` input.
const TRANSFORM_MODIFIER_TYPE_IDS: &[&str] = &["node.transform_shake"];
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
    /// The camera item's controls: the camera atom (curated shapes only),
    /// its lens and the cinematic tail.
    pub camera_controls: Vec<NodeId>,
    pub environment: EnvironmentVm,
    pub atmosphere: AtmosphereVm,
    /// The World item's controls: the render_scene root, physics worlds,
    /// the environment and the atmosphere.
    pub world_controls: Vec<NodeId>,
    /// Scene bounds for translate-slider range derivation. `Some((min, max))`
    /// when the graph stores import-time bounds (populated by the glTF importer
    /// from `GltfImportSummary`), read at VM-build time to compute scene-relative
    /// slider ranges. Fallback chain in `from_def`: camera-distance proxy →
    /// descriptor defaults.
    pub scene_bounds: Option<([f32; 3], [f32; 3])>,
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

/// Which material map port a layer skin is wired into.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkinTargetMap {
    Emissive,
    BaseColor,
}

impl SkinTargetMap {
    fn port_name(self) -> &'static str {
        match self {
            SkinTargetMap::Emissive => "emissive_map",
            SkinTargetMap::BaseColor => "base_color_map",
        }
    }
}

/// P4b: one layer-skin binding discovered on a scene object.
#[derive(Debug, Clone, PartialEq)]
pub struct SkinVm {
    /// The `node.layer_source` producer's doc id — the address the Skin row
    /// writes the `layer` param at.
    pub source_node_id: u32,
    /// The scope path to the level that actually contains `source_node_id`
    /// (the scene_object's own scope, which may be a group).
    pub source_node_scope_path: Vec<u32>,
    /// The bound layer id, read from the source node's `layer` param. `None`
    /// when the param is empty/absent (the row shows "None").
    pub source_layer_id: Option<String>,
    /// Which map port the source is wired into.
    pub target_map: SkinTargetMap,
    /// `true` when `source_layer_id` is set but the id doesn't exist in the
    /// project's current layer list (D8: loud missing-layer chip).
    pub source_missing: bool,
}

/// Payload for [`SceneObjectVm::Known`], boxed at the enum site for the same
/// clippy `large_enum_variant` reason as [`LightRow`]/[`OrbitCameraRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct SceneObjectKnownRow {
    /// A virtual parent for the material draws of one imported static model.
    pub is_group: bool,
    /// Children follow their parent in `SceneVm.objects`; indices still refer
    /// to the physical render slots used by graph editing commands.
    pub parent_group_id: Option<u32>,
    pub index: usize,
    /// Stable selection identity: the group id for a virtual parent, otherwise
    /// the `node.scene_object`'s own doc id — the address
    /// `RenameSceneObjectCommand`/the eye-toggle write take, and the same
    /// value `group_node_id` resolved to pre-D12 when an object happened to
    /// be grouped.
    pub object_node_id: u32,
    /// The stable identity behind `object_node_id`.
    pub object: NodeId,
    /// The family child's platonic mesh. Water looks expose this node's
    /// radius as their Size control; the simulation-owning Water row leaves
    /// it empty because its geometry comes from the liquid surface.
    pub look_mesh: Option<NodeId>,
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
    /// Chain of single-Transform-in/Transform-out nodes between the
    /// `node.transform_3d` source and the scene_object's `transform` input,
    /// in wire order (P3). Empty when there are no transform modifiers.
    pub transform_chain: Vec<ModifierVm>,
    /// `false` when the scene_object's `transform` chain couldn't be walked
    /// at all — mirrors `modifier_chain_parseable` for the transform wire.
    pub transform_chain_parseable: bool,
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
    /// P4b: the object's layer skin, discovered from a `node.layer_source`
    /// wired into `emissive_map` or `base_color_map`. `None` when neither
    /// map has a layer_source producer.
    pub skin: Option<SkinVm>,
    /// Standard physics discovered on this object. The body is inside the
    /// object group for imported models and at root for hand-built objects.
    pub physics: Option<PhysicsVm>,
    pub physics_imported: bool,
    /// The liquid domain this object's surface is built from
    /// (`manifold_core::liquid_domain::liquid_domain_of`, the walk forces and
    /// pairing use).
    pub liquid_domain: Option<SceneNodeRef>,
    /// The liquid domain, its transforms and the object-owned role nodes:
    /// their controls belong to this object.
    pub fluid_controls: Vec<NodeId>,
    /// Static domain bounds when the fluid domain is fully authored by
    /// unwired scalar/transform parameters.
    pub fluid_domain: Option<FluidDomainLayout>,
    /// The domain transform's addresses and current values, independent of
    /// the visible mesh object's ordinary transform.
    pub fluid_domain_transform: Option<TransformVm>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct PhysicsVm {
    pub body_node_id: u32,
    pub body: NodeId,
    pub body_scope_path: Vec<u32>,
    pub enabled: bool,
    pub imported: bool,
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
    pub node: NodeId,
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
    pub node: NodeId,
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
    pub node: NodeId,
    /// The scope this material atom's params write at — empty for a root/
    /// ungrouped object, `[group_node_id]` for one living inside an object's
    /// group (or, on the rare crossed-group shape, one level deeper). Kept
    /// as addressing identity (not a param value) — the sole source of the
    /// scope once `base_color_addr` no longer exists.
    pub scope_path: Vec<u32>,
    /// `true` only for `node.pbr_material` — metallic/roughness is a
    /// PBR-only concept, so an unlit/cel material's quick knobs are
    /// base color alone (D4: "the atom's own params otherwise").
    pub is_pbr: bool,
    /// The fixed map-input vocabulary exposed by the material inspector.
    pub texture_slots: Vec<MaterialTextureSlot>,
    /// Number of scene objects sharing this material identity when the whole
    /// scene is fully classified; `None` means the count is not authoritative.
    pub shared_object_count: Option<usize>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MaterialTextureSlot {
    pub port: String,
    pub source: MaterialTextureSource,
}

#[derive(Debug, Clone, PartialEq)]
pub enum MaterialTextureSource {
    Unconnected,
    Known {
        scope_path: Vec<u32>,
        node_doc_id: u32,
        type_id: String,
    },
    GraphSource,
}

const MATERIAL_TEXTURE_PORTS: &[&str] = &[
    "base_color_map",
    "normal_map",
    "mr_map",
    "occlusion_map",
    "emissive_map",
    "sheen_color_map",
    "sheen_roughness_map",
    "iridescence_map",
    "iridescence_thickness_map",
    "anisotropy_map",
    "clearcoat_map",
    "clearcoat_roughness_map",
    "clearcoat_normal_map",
    "specular_map",
    "specular_color_map",
    "transmission_map",
    "diffuse_transmission_map",
    "diffuse_transmission_color_map",
    "volume_thickness_map",
];

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
    pub node: NodeId,
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

/// Payload for [`CameraVm::Loop`] (SCENE_LOOP_DESIGN D3).
#[derive(Debug, Clone, PartialEq)]
pub struct LoopCameraRow {
    pub node_doc_id: u32,
    pub lens: Option<LensRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CameraVm {
    None,
    Orbit(Box<OrbitCameraRow>),
    Free(Box<FreeCameraRow>),
    LookAt(Box<LookAtCameraRow>),
    Loop(Box<LoopCameraRow>),
    /// An uncurated producer (including a modifier switch) can still feed
    /// a recognized downstream lens and cinematic tail.
    Custom { node_doc_id: u32, lens: Option<LensRow> },
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

/// Read a string param, coercing only the exact string variant. Every other
/// shape (including the Float placeholder `node.layer_source.layer` ships as a
/// default) returns `None` so the UI falls back to "None".
fn param_string(node: &EffectGraphNode, name: &str) -> Option<String> {
    match node.params.get(name) {
        Some(SerializedParamValue::String { value }) => Some(value.clone()),
        _ => None,
    }
}

impl SceneVm {
    /// Discover the full D3 trace for `def`. `None` when no `render_scene`
    /// reaches the graph's output (the empty-scene case; D7 handles it).
    /// Backwards-compatible entry point: no project layer list, so missing-
    /// source detection is disabled (used by tests and other call sites that
    /// don't have the project handy).
    pub fn from_def(def: &EffectGraphDef) -> Option<SceneVm> {
        Self::from_def_with_layers(def, &[])
    }

    /// P4b variant: pass the current project layer list so the VM can flag a
    /// skin whose `layer` id no longer resolves (D8 missing-layer chip). Tests
    /// and other callers without a layer list can use [`Self::from_def`].
    pub fn from_def_with_layers(def: &EffectGraphDef, layer_ids: &[LayerId]) -> Option<SceneVm> {
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

        let layer_id_set: HashSet<&str> = layer_ids.iter().map(|id| id.as_ref()).collect();
        // A graph the index refuses has no stable paths, so nothing in it is
        // recognised as water; forces and pairing refuse the same graph.
        let index = FlatSceneIndex::build(def).ok();
        let (objects, vertex_count, vertex_count_exact) =
            trace_objects(&root, scene_node, &layer_id_set, index.as_ref());
        let lights = trace_lights(&root, scene_node);
        let camera = trace_camera(&root, scene_node);
        let camera_controls = camera_controls(&root, &camera);
        let environment = trace_environment(&root, scene_node);
        let atmosphere = trace_atmosphere(&root, scene_node);
        let world_controls = world_controls(&root, scene_node, &environment, &atmosphere);
        let object_count = objects.iter().filter(|row| !matches!(row, SceneObjectVm::Known(row) if row.parent_group_id.is_some())).count();
        let light_count = lights.len();
        let shadow_caster_count = lights.iter().filter(|l| light_casts_shadows(&root, l)).count();

        // Scene bounds extraction with fallback chain (SCENE_PANEL_UX_DESIGN.md):
        // 1. Stored bounds from import-time AABB (glTF importer populates this)
        // 2. Camera-distance proxy for scenes imported before this field existed
        // 3. None → caller falls back to descriptor defaults
        let scene_bounds = def.preset_metadata.as_ref().and_then(|meta| meta.scene_bounds)
            .or_else(|| {
                // Fallback 2: camera-distance proxy. The BUG-774a import math sets
                // orbit camera distance = 2.2 × radius, so radius ≈ distance/2.2.
                // Read the camera node's distance param and synthesize bounds centered
                // on the camera target (if exposed) or origin. Conservative heuristic
                // for legacy projects.
                let camera = &camera;
                let known_camera = match camera {
                    CameraVm::Orbit(row) => row,
                    CameraVm::None | CameraVm::Custom { .. } | CameraVm::Free(_) | CameraVm::LookAt(_) | CameraVm::Loop(_) => return None,
                };

                let camera_node = root.node(known_camera.node_doc_id)?;
                let distance = param_f32(camera_node, "distance", 0.0);
                if distance <= 0.0 {
                    return None;
                }
                // Recover radius from distance (inverse of BUG-774a math: distance = 2.2 * radius)
                let radius = distance / 2.2;

                // Try to read camera target (default to origin if not exposed)
                let target = [
                    param_f32(camera_node, "target_x", 0.0),
                    param_f32(camera_node, "target_y", 0.0),
                    param_f32(camera_node, "target_z", 0.0),
                ];

                // Synthesize bounds as a cube centered on target with extent = radius
                let bounds_min = [target[0] - radius, target[1] - radius, target[2] - radius];
                let bounds_max = [target[0] + radius, target[1] + radius, target[2] + radius];

                Some((bounds_min, bounds_max))
            });

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
            camera_controls,
            environment,
            atmosphere,
            world_controls,
            scene_bounds,
        })
    }
}

/// Stable identities of `ids` in `level`; nodes without one own no controls.
fn stable_ids(level: &Level, ids: impl IntoIterator<Item = u32>) -> Vec<NodeId> {
    ids.into_iter()
        .filter_map(|id| level.node(id))
        .map(|node| node.node_id.clone())
        .filter(|id| !id.is_empty())
        .collect()
}

/// The first node of `type_id` anywhere in `nodes`, group bodies included.
fn first_of_type<'a>(nodes: &'a [EffectGraphNode], type_id: &str) -> Option<&'a EffectGraphNode> {
    nodes.iter().find_map(|node| {
        if node.type_id == type_id {
            return Some(node);
        }
        node.group.as_deref().and_then(|group| first_of_type(&group.nodes, type_id))
    })
}

/// The camera atom (curated shapes only), its lens, and, when a lens is
/// wired, the cinematic tail's motion blur and bokeh wherever they live.
fn camera_controls(level: &Level, camera: &CameraVm) -> Vec<NodeId> {
    let (atom, lens) = match camera {
        CameraVm::Orbit(c) => (Some(c.node_doc_id), c.lens.as_ref()),
        CameraVm::Free(c) => (Some(c.node_doc_id), c.lens.as_ref()),
        CameraVm::LookAt(c) => (Some(c.node_doc_id), c.lens.as_ref()),
        CameraVm::Loop(c) => (None, c.lens.as_ref()),
        CameraVm::Custom { lens, .. } => (None, lens.as_ref()),
        CameraVm::None => (None, None),
    };
    let mut controls = stable_ids(level, atom.into_iter().chain(lens.map(|lens| lens.node_doc_id)));
    if lens.is_some() {
        controls.extend(
            [MOTION_BLUR_TYPE_ID, BOKEH_GATHER_TYPE_ID]
                .into_iter()
                .filter_map(|type_id| first_of_type(level.nodes, type_id))
                .map(|node| node.node_id.clone())
                .filter(|id| !id.is_empty()),
        );
    }
    controls
}

/// The render_scene root (its Rendering toggles), every physics world, the
/// environment and the atmosphere.
fn world_controls(
    level: &Level,
    scene_node: &EffectGraphNode,
    environment: &EnvironmentVm,
    atmosphere: &AtmosphereVm,
) -> Vec<NodeId> {
    let mut ids = vec![scene_node.id];
    ids.extend(level.nodes.iter().filter(|node| node.type_id == "node.physics_world").map(|node| node.id));
    match environment {
        EnvironmentVm::Importer(e) => ids.push(e.bake_node_id),
        EnvironmentVm::Bare(e) => ids.push(e.node_doc_id),
        EnvironmentVm::Custom { .. } | EnvironmentVm::None => {}
    }
    if let AtmosphereVm::Wired(a) = atmosphere {
        ids.push(a.node_doc_id);
    }
    stable_ids(level, ids)
}

fn light_casts_shadows(level: &Level, light: &SceneLightVm) -> bool {
    let SceneLightVm::Known(row) = light else {
        return false;
    };
    let Some(node) = level.node(row.node_doc_id) else {
        return false;
    };
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
    output_port: &str,
) -> Option<(&'a EffectGraphNode, Level<'a>)> {
    let inner = Level { nodes: &group.nodes, wires: &group.wires };
    let (producer_id, _) = inner.nodes.iter().filter(|n| n.type_id == GROUP_OUTPUT_TYPE_ID)
        .find_map(|out| inner.producer(out.id, output_port))?;
    let node = inner.node(producer_id)?;
    (node.type_id == SCENE_OBJECT_TYPE_ID).then_some((node, inner))
}

/// Resolve `to_node`'s `to_port` producer, transparently crossing ONE level
/// of `GROUP_TYPE_ID` boundary when the direct producer is a bare group
/// re-exporting the same-named port from its own `system.group_output` —
/// the shape `migrate_scene_object_wires` produces for every pre-existing
/// (migrated) project: the minted `node.scene_object` stays a ROOT-level
/// sibling of the mesh producer's group rather than nested inside it (D5's
/// "same-scope re-point", confirmed against the shipped `Scene.json`:
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

fn material_texture_slots(
    level: &Level<'_>,
    scope_path: &[u32],
    object_node_id: u32,
) -> Vec<MaterialTextureSlot> {
    MATERIAL_TEXTURE_PORTS
        .iter()
        .map(|&port| {
            let source = if level.producer(object_node_id, port).is_none() {
                MaterialTextureSource::Unconnected
            } else if let Some((_level, crossed_group, node, _)) =
                resolve_producer_through_group(level, object_node_id, port)
            {
                let mut source_scope = scope_path.to_vec();
                if let Some(group_id) = crossed_group {
                    source_scope.push(group_id);
                }
                MaterialTextureSource::Known {
                    scope_path: source_scope,
                    node_doc_id: node.id,
                    type_id: node.type_id.clone(),
                }
            } else {
                MaterialTextureSource::GraphSource
            };
            MaterialTextureSlot {
                port: port.to_string(),
                source,
            }
        })
        .collect()
}

fn assign_shared_material_counts(objects: &mut [SceneObjectVm]) {
    let mut counts: HashMap<(Vec<u32>, u32), usize> = HashMap::new();
    let mut complete = true;
    for object in objects.iter() {
        let SceneObjectVm::Known(row) = object else {
            complete = false;
            continue;
        };
        if row.is_group { continue; }
        let MaterialVm::Known(material) = &row.material else {
            complete = false;
            continue;
        };
        *counts
            .entry((material.scope_path.clone(), material.node_doc_id))
            .or_default() += 1;
    }
    for object in objects {
        let SceneObjectVm::Known(row) = object else {
            continue;
        };
        let MaterialVm::Known(material) = &mut row.material else {
            continue;
        };
        material.shared_object_count = complete.then(|| {
            counts
                .get(&(material.scope_path.clone(), material.node_doc_id))
                .copied()
                .unwrap_or(0)
        });
    }
}

/// BUG-194 (D4) + D12: alongside the objects themselves, returns the summed
/// vertex count and whether that sum is exact (`true`) or a lower bound
/// (`false` — at least one object's mesh source didn't resolve to a known
/// count, e.g. a hand-wired procedural generator outside the closed-form
/// table, or an unparseable chain).
/// The liquid domain behind render slot `slot`, found by the core walk and
/// located in the authored graph.
struct LiquidDomainAt<'a> {
    level: Level<'a>,
    /// Group doc ids from the root down to `level`.
    scope: Vec<u32>,
    node: &'a EffectGraphNode,
    stable: SceneNodeRef,
    /// Whether the object's mesh walk should contribute display controls to
    /// the Water row. Particle View's display mesh is family dressing, not a
    /// simulation control.
    surface_controls: bool,
}

struct WaterFamilyInfo {
    water_object_id: u32,
    domain_node_id: u32,
    /// Child object ids in the family-output order: Foam, Spray, Bubbles.
    output_order: HashMap<u32, usize>,
}

/// Recognise the authored Water family from its resolved group outputs. A
/// generic imported compound remains on the legacy path unless it has all
/// four family outputs and exactly one liquid-domain simulation owner in its
/// group body.
/// Keep aligned with manifold-editing commands::graph::scene::is_water_family_parent:
/// four distinct object outputs (water, foam, spray, bubbles), with water
/// reaching the group's sole liquid domain. Renderer has no production editing
/// dependency; app's water_family_recognizers_agree test checks both.
fn discover_water_families(
    level: &Level<'_>,
    scene_node: &EffectGraphNode,
    objects: usize,
) -> HashMap<u32, WaterFamilyInfo> {
    let output_roles = ["object", "object_1", "object_2", "object_3"];
    let mut candidates: HashMap<u32, HashMap<&str, u32>> = HashMap::new();
    for slot in 0..objects {
        let Some((group_id, output_port)) = level.producer(scene_node.id, &format!("object_{slot}")) else {
            continue;
        };
        let Some(group_node) = level.node(group_id).filter(|node| node.type_id == GROUP_TYPE_ID) else {
            continue;
        };
        let Some((object, _)) = group_node
            .group
            .as_deref()
            .and_then(|group| find_scene_object_in_group(group, output_port))
        else {
            continue;
        };
        candidates.entry(group_id).or_default().insert(output_port, object.id);
    }

    candidates
        .into_iter()
        .filter_map(|(group_id, members)| {
            if output_roles.iter().any(|role| !members.contains_key(role)) {
                return None;
            }
            let mut objects_seen = HashSet::new();
            if !output_roles.iter().all(|role| objects_seen.insert(members[role])) {
                return None;
            }
            let group = level.node(group_id)?.group.as_deref()?;
            let mut domain_nodes = group.nodes.iter().filter(|node| is_liquid_domain(&node.type_id));
            let domain_node_id = domain_nodes.next()?.id;
            if domain_nodes.next().is_some() {
                return None;
            }
            let water_object_id = members["object"];
            let group_node = level.node(group_id)?;
            // Particle View's water vertices come from a platonic mesh, so
            // liquid_domain_of cannot see the solver through vertices. The
            // object still has a structural path to the sole domain through
            // frame → copies → instances; require that path instead of
            // attributing an unrelated domain to the family by coincidence.
            if !family_owner_reaches_domain(group_node, water_object_id, domain_node_id) {
                return None;
            }
            let output_order = output_roles
                .iter()
                .enumerate()
                .skip(1)
                .map(|(order, role)| (members[*role], order))
                .collect();
            Some((
                group_id,
                WaterFamilyInfo {
                    water_object_id,
                    domain_node_id,
                    output_order,
                },
            ))
        })
        .collect()
}

fn family_owner_reaches_domain(
    group_node: &EffectGraphNode,
    object_id: u32,
    domain_node_id: u32,
) -> bool {
    let Some(group) = group_node.group.as_deref() else { return false; };
    let mut pending = vec![object_id];
    let mut seen = HashSet::new();
    while let Some(target) = pending.pop() {
        if !seen.insert(target) { continue; }
        for wire in group.wires.iter().filter(|wire| wire.to_node == target) {
            if wire.from_node == domain_node_id { return true; }
            pending.push(wire.from_node);
        }
    }
    false
}

fn family_liquid_domain<'a>(
    root: &Level<'a>,
    group_id: u32,
    domain_node_id: u32,
) -> Option<LiquidDomainAt<'a>> {
    let group_node = root.node(group_id)?;
    let group = group_node.group.as_deref()?;
    let level = Level { nodes: &group.nodes, wires: &group.wires };
    let node = level.node(domain_node_id)?;
    Some(LiquidDomainAt {
        level,
        scope: vec![group_id],
        node,
        stable: SceneNodeRef {
            scope: vec![group_node.node_id.clone()],
            node: node.node_id.clone(),
        },
        surface_controls: false,
    })
}

/// Resolve a Water look's mesh source inside the family group. Child looks
/// are intentionally simple platonic-mesh → scene-object chains, but tolerate
/// a known mesh modifier or transparent nested group so the row stays honest
/// when a preset is hand-edited.
fn family_look_mesh(group_node: &EffectGraphNode, object_id: u32) -> Option<NodeId> {
    let group = group_node.group.as_deref()?;
    let mut level = Level { nodes: &group.nodes, wires: &group.wires };
    let mut cursor = level.producer(object_id, "vertices");
    let mut guard = 0;
    while let Some((node_id, port)) = cursor {
        guard += 1;
        if guard > 64 { return None; }
        let node = level.node(node_id)?;
        if node.type_id == "node.platonic_solid_mesh" {
            return Some(node.node_id.clone());
        }
        if node.type_id == GROUP_TYPE_ID {
            let nested = node.group.as_deref()?;
            let inner = Level { nodes: &nested.nodes, wires: &nested.wires };
            let output = inner.nodes.iter().find(|candidate| candidate.type_id == GROUP_OUTPUT_TYPE_ID)?;
            let (inner_id, inner_port) = inner.producer(output.id, port)?;
            level = inner;
            cursor = Some((inner_id, inner_port));
        } else if MODIFIER_TYPE_IDS.contains(&node.type_id.as_str()) {
            cursor = level.producer(node.id, "in");
        } else {
            return None;
        }
    }
    None
}

fn slot_liquid_domain<'a>(
    root: &Level<'a>,
    index: Option<&FlatSceneIndex>,
    scene_node: &EffectGraphNode,
    slot: usize,
) -> Option<LiquidDomainAt<'a>> {
    let index = index?;
    let scene = SceneNodeRef { scope: Vec::new(), node: scene_node.node_id.clone() };
    let object = index.scene_object_at(&scene, slot as u32).ok()??;
    let domain = liquid_domain_of(index, &object).ok()??;
    let mut level = Level { nodes: root.nodes, wires: root.wires };
    let mut scope = Vec::with_capacity(domain.scope.len());
    for group_id in &domain.scope {
        let group = level.nodes.iter().find(|node| &node.node_id == group_id)?;
        let body = group.group.as_deref()?;
        scope.push(group.id);
        level = Level { nodes: &body.nodes, wires: &body.wires };
    }
    let node = level.nodes.iter().find(|node| node.node_id == domain.node)?;
    Some(LiquidDomainAt { level, scope, node, stable: domain, surface_controls: true })
}

fn trace_objects(
    level: &Level,
    scene_node: &EffectGraphNode,
    layer_id_set: &HashSet<&str>,
    index: Option<&FlatSceneIndex>,
) -> (Vec<SceneObjectVm>, u64, bool) {
    let objects = param_f32(scene_node, "objects", 0.0).max(0.0) as usize;
    let water_families = discover_water_families(level, scene_node, objects);
    let mut vertex_count: u64 = 0;
    let mut vertex_count_exact = true;
    let mut out = Vec::with_capacity(objects);
    let mut seen_groups = HashSet::new();
    for k in 0..objects {
        let port = format!("object_{k}");
        let liquid = slot_liquid_domain(level, index, scene_node, k).or_else(|| {
            let (group_id, output_port) = level.producer(scene_node.id, &port)?;
            let family = water_families.get(&group_id)?;
            (output_port == "object").then(|| family_liquid_domain(level, group_id, family.domain_node_id))?
        });
        let (mut row, source_vertex_count) = match level.producer(scene_node.id, &port) {
            Some((producer_id, output_port)) => match level.node(producer_id) {
                Some(producer_node) if producer_node.type_id == SCENE_OBJECT_TYPE_ID => {
                    trace_scene_object(level, Vec::new(), producer_node, None, k, layer_id_set, liquid)
                }
                Some(producer_node) if producer_node.type_id == GROUP_TYPE_ID => {
                    match producer_node
                        .group
                        .as_deref()
                        .and_then(|group| find_scene_object_in_group(group, output_port))
                    {
                        Some((inner_node, inner_level)) => trace_scene_object(
                            &inner_level,
                            vec![producer_id],
                            inner_node,
                            Some(producer_id),
                            k,
                            layer_id_set,
                            liquid,
                        ),
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
        if let SceneObjectVm::Known(child) = &mut row
            && let Some(group_id) = child.group_node_id
            && let Some(group_node) = level.node(group_id)
            && let Some(group) = group_node.group.as_ref()
        {
            let inner = Level { nodes: &group.nodes, wires: &group.wires };
            if !water_families.contains_key(&group_id)
                && let Some((parent_source, _)) = inner.producer(child.object_node_id, "parent_transform")
            {
                if seen_groups.insert(group_id) {
                    let mut parent = child.clone();
                    parent.is_group = true;
                    parent.object_node_id = group_id;
                    parent.object = group_node.node_id.clone();
                    parent.name = group_node.handle.clone().unwrap_or_else(|| "Model".into());
                    parent.visible_addr.param_id = "parent_visible".into();
                    parent.visible_value = inner.node(child.object_node_id)
                        .is_none_or(|node| param_f32(node, "parent_visible", 1.0) > 0.5);
                    parent.visible_driven = inner.producer(child.object_node_id, "parent_visible").is_some();
                    let authored = inner.node(parent_source)
                        .filter(|node| node.type_id == "node.transform_3d")
                        .map(|node| node.id)
                        .or_else(|| group_body_id(&inner).and_then(|body| inner.producer(body, "transform").map(|(id, _)| id)));
                    parent.transform = authored.map(|id| trace_transform(&inner, vec![group_id], id));
                    parent.material = MaterialVm::None;
                    parent.skin = None;
                    parent.transform_chain.clear();
                    parent.modifier_chain.clear();
                    out.push(SceneObjectVm::Known(parent));
                }
                child.parent_group_id = Some(group_id);
                child.physics = None;
                child.physics_imported = false;
            }
        }
        out.push(row);
    }
    for (group_id, family) in &water_families {
        let Some(group_node) = level.node(*group_id) else { continue; };
        for object in &mut out {
            let SceneObjectVm::Known(row) = object else { continue; };
            if row.group_node_id != Some(*group_id) { continue; }
            if row.object_node_id == family.water_object_id {
                // Water is the physical row itself. Its object identity stays
                // the scene_object doc id so selection, rename and deletion
                // address the actual render slot rather than a virtual group.
                row.is_group = true;
                row.parent_group_id = None;
                row.look_mesh = None;
                let Some(group) = group_node.group.as_ref() else { continue; };
                let inner = Level { nodes: &group.nodes, wires: &group.wires };
                row.visible_addr.param_id = "parent_visible".into();
                row.visible_value = inner
                    .node(row.object_node_id)
                    .is_none_or(|node| param_f32(node, "parent_visible", 1.0) > 0.5);
                row.visible_driven = inner.producer(row.object_node_id, "parent_visible").is_some();
            } else if family.output_order.contains_key(&row.object_node_id) {
                // The other outputs are looks: they own material, Size and
                // their local eye only. Simulation and object-editing fields
                // belong exclusively to Water.
                row.parent_group_id = Some(family.water_object_id);
                row.look_mesh = family_look_mesh(group_node, row.object_node_id);
                row.transform = None;
                row.transform_chain.clear();
                row.transform_chain_parseable = false;
                row.modifier_chain.clear();
                row.modifier_chain_parseable = false;
                row.skin = None;
                row.physics = None;
                row.physics_imported = false;
                row.liquid_domain = None;
                row.fluid_controls.clear();
                row.fluid_domain = None;
                row.fluid_domain_transform = None;
            }
        }
    }
    // A duplicated child may occupy a later render slot, after another group.
    // Keep the outliner in parent/children order without changing physical slots.
    let group_indices: HashMap<_, _> = out.iter().filter_map(|row| match row {
        SceneObjectVm::Known(row) if row.is_group => Some((row.object_node_id, row.index)),
        _ => None,
    }).collect();
    let family_order: HashMap<(u32, u32), usize> = water_families
        .iter()
        .flat_map(|(group_id, family)| {
            std::iter::once((*group_id, family.water_object_id, 0usize))
                .chain(family.output_order.iter().map(|(object_id, order)| (*group_id, *object_id, *order)))
        })
        .map(|(group_id, object_id, order)| ((group_id, object_id), order))
        .collect();
    out.sort_by_key(|row| match row {
        SceneObjectVm::Known(row) => {
            let group_order = row
                .group_node_id
                .and_then(|group_id| family_order.get(&(group_id, row.object_node_id)).copied());
            let anchor = row.parent_group_id
                .and_then(|id| group_indices.get(&id).copied())
                .unwrap_or(row.index);
            (anchor, row.parent_group_id.is_some(), group_order.unwrap_or(row.index), row.index)
        }
        SceneObjectVm::Custom { index } => (*index, false, *index, *index),
    });
    assign_shared_material_counts(&mut out);
    (out, vertex_count, vertex_count_exact)
}

/// Walk the P3 Transform modifier chain feeding `scene_object.transform`,
/// backward to the `node.transform_3d` source. Collects any
/// `TRANSFORM_MODIFIER_TYPE_IDS` atoms in wire order (source → … → object)
/// and returns the traced `TransformVm` plus parseability. Mirrors the mesh
/// modifier walk's group-crossing tolerance: a bare group that re-exports
/// `transform` is transparent, not a modifier itself.
fn walk_transform_chain(
    level: &Level,
    scope_path: Vec<u32>,
    object_node_id: u32,
) -> (Option<TransformVm>, Vec<ModifierVm>, bool) {
    let mut chain = Vec::new();
    let mut current_level = Level { nodes: level.nodes, wires: level.wires };
    let mut sp = scope_path;
    let mut cursor = current_level.producer(object_node_id, "transform");
    let mut parseable = cursor.is_some();
    let mut transform_vm = None;
    let mut guard = 0;
    while let Some((node_id, port)) = cursor {
        guard += 1;
        if guard > 64 {
            parseable = false; // cycle guard.
            break;
        }
        let Some(n) = current_level.node(node_id) else {
            parseable = false;
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
            let Some((inner_id, inner_port)) = inner.producer(out_node.id, port) else {
                parseable = false;
                break;
            };
            sp.push(node_id);
            current_level = inner;
            cursor = Some((inner_id, inner_port));
            continue;
        }
        if n.type_id == TRANSFORM_3D_TYPE_ID {
            transform_vm = Some(trace_transform(&current_level, sp, n.id));
            break;
        }
        if n.type_id == manifold_core::effect_graph_def::GROUP_INPUT_TYPE_ID && port == "pose" {
            cursor = group_body_id(&current_level)
                .and_then(|id| current_level.producer(id, "transform"));
            parseable = false;
            continue;
        }
        if n.type_id == "node.physics_world" {
            // A pose is paired with one description input. Follow that body's
            // authored transform for editing; the live simulated pose is not a
            // project value and must never be written back by the scene panel.
            cursor = port.strip_prefix("pose_")
                .and_then(|index| current_level.producer(n.id, &format!("body_{index}")));
            parseable = false; // transform modifiers must not splice across the solver.
            continue;
        }
        if n.type_id == "node.rigid_body" {
            cursor = current_level.producer(n.id, "transform");
            continue;
        }
        if TRANSFORM_MODIFIER_TYPE_IDS.contains(&n.type_id.as_str()) {
            chain.push(ModifierVm { node_doc_id: n.id, node: n.node_id.clone(), type_id: n.type_id.clone() });
            cursor = current_level.producer(n.id, "transform");
            continue;
        }
        parseable = false; // producer is neither transform_3d nor a known modifier.
        break;
    }
    chain.reverse(); // wire order: transform_3d → … → scene_object.
    let transform = transform_vm;
    // A chain of Transform modifiers that never reaches a transform_3d
    // source is not a complete, splicable stack.
    let parseable = parseable && transform.is_some();
    (transform, chain, parseable)
}

fn physics_vm(
    level: &Level<'_>,
    scope_path: &[u32],
    object_id: u32,
    group_node_id: Option<u32>,
) -> Option<PhysicsVm> {
    let (body_id, body_scope, imported) = if group_node_id.is_some() {
        let body_id = group_body_id(level)?;
        (
            body_id,
            scope_path.to_vec(),
            mesh_source_is_gltf(level, object_id),
        )
    } else {
        (
            physics_body_in_level(level, object_id)?,
            scope_path.to_vec(),
            mesh_source_is_gltf(level, object_id),
        )
    };
    let body = level.node(body_id)?;
    Some(PhysicsVm {
        body_node_id: body_id,
        body: body.node_id.clone(),
        body_scope_path: body_scope,
        enabled: param_bool(body, "enabled", true),
        imported,
    })
}

fn param_bool(node: &EffectGraphNode, name: &str, default: bool) -> bool {
    match node.params.get(name) {
        Some(SerializedParamValue::Bool { value }) => *value,
        _ => default,
    }
}

fn group_body_id(level: &Level<'_>) -> Option<u32> {
    level.nodes.iter().filter(|n| n.type_id == GROUP_OUTPUT_TYPE_ID).find_map(|output| {
        let (body, _) = level.producer(output.id, "body")?;
        (level.node(body)?.type_id == "node.rigid_body").then_some(body)
    })
}

fn mesh_source_is_gltf(level: &Level<'_>, object_id: u32) -> bool {
    // Eligibility matches the command: directly rendered rigid scan geometry.
    level
        .producer(object_id, "vertices")
        .and_then(|(id, _)| level.node(id))
        .is_some_and(|n| n.type_id == "node.gltf_mesh_source")
}

fn physics_body_in_level(level: &Level<'_>, object_id: u32) -> Option<u32> {
    let (world_id, port) = level.producer(object_id, "transform")?;
    let world = level.node(world_id)?;
    if world.type_id != "node.physics_world" { return None; }
    let input = format!("body_{}", port.strip_prefix("pose_")?);
    let (body_id, _) = level.producer(world_id, &input)?;
    (level.node(body_id)?.type_id == "node.rigid_body").then_some(body_id)
}

/// Resolve the editable domain transform and derive bounds only from a
/// completely authored, non-driven domain. A graph-driven transform or
/// scalar solver setting has no stable editor bounds to expose.
fn trace_fluid_domain(
    level: &Level<'_>,
    scope_path: &[u32],
    fluid: &EffectGraphNode,
) -> (Option<FluidDomainLayout>, Option<TransformVm>) {
    let domain_wire = level.producer(fluid.id, "domain").is_some();
    let domain_size_driven = level.producer(fluid.id, "domain_size").is_some();
    let resolution_driven = level.producer(fluid.id, "resolution").is_some();
    let domain = resolve_producer_through_group(level, fluid.id, "domain")
        .filter(|(_, _, node, _)| node.type_id == TRANSFORM_3D_TYPE_ID);

    let Some((domain_level, crossed_group, domain_node, _)) = domain else {
        if domain_wire || domain_size_driven || resolution_driven {
            return (None, None);
        }
        let settings = FluidSettings {
            resolution: param_f32(fluid, "resolution", 24.0).round() as u32,
            domain_size: param_f32(fluid, "domain_size", 4.0),
            ..FluidSettings::default()
        };
        return (settings.domain_layout().ok(), None);
    };

    let mut domain_scope = scope_path.to_vec();
    if let Some(group_id) = crossed_group {
        domain_scope.push(group_id);
    }
    let transform = trace_transform(&domain_level, domain_scope, domain_node.id);
    let transform_driven = transform.pos_driven.0
        || transform.pos_driven.1
        || transform.pos_driven.2
        || transform.rot_driven.0
        || transform.rot_driven.1
        || transform.rot_driven.2
        || transform.scale_driven.0
        || transform.scale_driven.1
        || transform.scale_driven.2;
    let billboard = param_bool(domain_node, "billboard", false);
    let billboard_driven = domain_level.producer(domain_node.id, "billboard").is_some();
    if resolution_driven || transform_driven || billboard || billboard_driven {
        return (None, Some(transform));
    }

    let settings = FluidSettings {
        resolution: param_f32(fluid, "resolution", 24.0).round() as u32,
        domain_size: param_f32(fluid, "domain_size", 4.0),
        domain: Some(Transform {
            pos: [transform.pos_value.0, transform.pos_value.1, transform.pos_value.2],
            rot_euler: [transform.rot_value.0, transform.rot_value.1, transform.rot_value.2],
            scale: [transform.scale_value.0, transform.scale_value.1, transform.scale_value.2],
            billboard: false,
        }),
        ..FluidSettings::default()
    };
    (settings.domain_layout().ok(), Some(transform))
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
    layer_id_set: &HashSet<&str>,
    liquid: Option<LiquidDomainAt<'_>>,
) -> (SceneObjectVm, Option<u32>) {
    let object_node_id = node.id;
    let object_scope_path = scope_path.clone();
    let name = node.handle.clone().unwrap_or_else(|| format!("Object {k}"));
    let visible_addr =
        ParamAddr { scope_path: scope_path.clone(), node_doc_id: object_node_id, param_id: "visible".to_string() };
    let visible_value = param_f32(node, "visible", 1.0) > 0.5;
    let visible_driven = level.producer(object_node_id, "visible").is_some();

    let (transform, transform_chain, transform_chain_parseable) =
        walk_transform_chain(level, scope_path.clone(), object_node_id);

    let material = resolve_producer_through_group(level, object_node_id, "material")
        .filter(|(_, _, n, _)| MATERIAL_TYPE_IDS.contains(&n.type_id.as_str()))
        .map(|(_lvl, crossed_group, n, _)| {
            let mut scope_path = scope_path.clone();
            if let Some(g) = crossed_group {
                scope_path.push(g);
            }
            let texture_slots = material_texture_slots(level, &object_scope_path, object_node_id);
            MaterialVm::Known(Box::new(MaterialColorRow {
                node_doc_id: n.id,
                node: n.node_id.clone(),
                scope_path,
                is_pbr: n.type_id == "node.pbr_material",
                texture_slots,
                shared_object_count: None,
            }))
        })
        .unwrap_or(MaterialVm::None);

    // P4b: discover a `node.layer_source` wired into `emissive_map` or
    // `base_color_map`. Emissive takes precedence if both are wired.
    let skin = [SkinTargetMap::Emissive, SkinTargetMap::BaseColor]
        .iter()
        .find_map(|&target| {
            resolve_producer_through_group(level, object_node_id, target.port_name())
                .filter(|(_, _, n, _)| n.type_id == "node.layer_source")
                .map(|(_lvl, crossed_group, n, _)| {
                    let mut source_node_scope_path = scope_path.clone();
                    if let Some(g) = crossed_group {
                        source_node_scope_path.push(g);
                    }
                    let source_layer_id = param_string(n, "layer").filter(|s| !s.is_empty());
                    let source_missing = source_layer_id
                        .as_ref()
                        .is_some_and(|id| !layer_id_set.contains(id.as_str()));
                    SkinVm {
                        source_node_id: n.id,
                        source_node_scope_path,
                        source_layer_id,
                        target_map: target,
                        source_missing,
                    }
                })
        });
    let physics_imported = mesh_source_is_gltf(level, object_node_id);
    let physics = physics_vm(level, &object_scope_path, object_node_id, group_node_id);

    // Modifier chain (D6, re-anchored per D12): walk backward from the
    // scene_object's OWN `vertices` input instead of a group output's
    // `vertices` port. `current_level` can switch mid-walk when the chain
    // crosses a bare-group boundary (the migrated-project shape,
    // `resolve_producer_through_group`'s doc comment) — a group is always
    // treated as a transparent re-export, never a modifier itself.
    let mut chain = Vec::new();
    let mut current_level = Level { nodes: level.nodes, wires: level.wires };
    let mut cursor = current_level.producer(object_node_id, "vertices");
    let mut mesh_scope_path = scope_path.clone();
    let mut parseable = cursor.is_some();
    let mut source_vertex_count: Option<u32> = None;
    let mut fluid_domain = None;
    let mut fluid_domain_transform = None;
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
            mesh_scope_path.push(node_id);
            cursor = Some((inner_id, inner_port));
            continue;
        }
        if !MODIFIER_TYPE_IDS.contains(&n.type_id.as_str()) {
            source_vertex_count = node_source_vertex_count(n);
            break; // reached the mesh source (or something un-curated) — stop, still parseable.
        }
        chain.push(ModifierVm { node_doc_id: n.id, node: n.node_id.clone(), type_id: n.type_id.clone() });
        cursor = current_level.producer(n.id, "in");
    }
    chain.reverse(); // wire order: source → … → scene_object.

    let mut fluid_controls: Vec<NodeId> = Vec::new();
    let mut own = |node: &EffectGraphNode| {
        if !node.node_id.is_empty() && !fluid_controls.contains(&node.node_id) {
            fluid_controls.push(node.node_id.clone());
        }
    };
    let liquid_domain = liquid.as_ref().map(|domain| domain.stable.clone());
    let surface_controls = liquid.as_ref().is_some_and(|domain| domain.surface_controls);
    if let Some(LiquidDomainAt { level: domain_level, scope: domain_scope, node: n, .. }) = liquid {
        (fluid_domain, fluid_domain_transform) = trace_fluid_domain(&domain_level, &domain_scope, n);
        own(n);
        for port in ["domain", "emitter", "initial_volume"] {
            if let Some((_, _, source, _)) = resolve_producer_through_group(&domain_level, n.id, port)
                && source.type_id == "node.transform_3d"
            {
                own(source);
            }
        }
        // The whitewater stepping on this domain's clock is the water's: the
        // legacy step reads its ticks, the per-tick step its epoch.
        let mut water_nodes = vec![n.id];
        for whitewater in domain_level.nodes.iter().filter(|node| node.type_id == "node.whitewater_step") {
            if ["ticks", "epoch"].iter().any(|port| domain_level.producer(whitewater.id, port).is_some_and(|(from, _)| from == n.id)) {
                own(whitewater);
                water_nodes.push(whitewater.id);
            }
        }
        // A shared value wired into the water's own nodes is the water's
        // control: the whitewater budget feeds the step and the state at once.
        for wire in domain_level.wires.iter().filter(|wire| water_nodes.contains(&wire.to_node)) {
            if let Some(value) = domain_level.node(wire.from_node).filter(|node| node.type_id == "node.value") {
                own(value);
            }
        }
        for index in 0..super::fluid_role::MAX_FLUID_ROLES {
            let role_port = format!("role_{index}");
            let Some((role_level, role_group, role, _)) =
                resolve_producer_through_group(&domain_level, n.id, &role_port)
            else {
                continue;
            };
            if role.type_id != "node.fluid_role_source" {
                continue;
            }
            // A visible source object owns its role controls. Only
            // standalone source groups belong in the liquid's panel.
            if role_group.is_some() && role_level.nodes.iter().any(|node| node.type_id == "node.scene_object") {
                continue;
            }
            own(role);
            for port in ["transform", "source_transform"] {
                if let Some((source_level, _, source, _)) = resolve_producer_through_group(&role_level, role.id, port)
                    && source.type_id == "node.transform_3d"
                    // A scene object drawing the same transform owns it: the
                    // box a collider role follows is the Obstacle's, not the
                    // water's.
                    && !source_level.nodes.iter().any(|object| {
                        object.type_id == "node.scene_object"
                            && source_level.producer(object.id, "transform").is_some_and(|(from, _)| from == source.id)
                    })
                {
                    own(source);
                }
            }
        }
    }

    if liquid_domain.is_some() && surface_controls {
        // Surface controls belong to the water whose mesh consumes them.
        // Stay inside this mesh level: group inputs are the ownership boundary
        // for simulation, collider and source objects in the enclosing scene.
        let mut pending: Vec<u32> = cursor.map(|(id, _)| id).into_iter().collect();
        let mut seen = HashSet::new();
        while let Some(id) = pending.pop() {
            if !seen.insert(id) { continue; }
            let Some(source) = current_level.node(id) else { continue; };
            if source.type_id == manifold_core::effect_graph_def::GROUP_INPUT_TYPE_ID
                || manifold_core::liquid_domain::is_liquid_domain(&source.type_id)
            { continue; }
            own(source);
            pending.extend(current_level.wires.iter().filter(|wire| wire.to_node == id)
                .map(|wire| wire.from_node));
        }
    }

    if group_node_id.is_some() {
        level.nodes.iter().filter(|node| node.type_id == "node.fluid_role_source").for_each(&mut own);
    }

    let row = SceneObjectVm::Known(Box::new(SceneObjectKnownRow {
        is_group: false,
        parent_group_id: None,
        index: k,
        object_node_id,
        object: node.node_id.clone(),
        look_mesh: None,
        group_node_id,
        name,
        visible_addr,
        visible_value,
        visible_driven,
        transform,
        material,
        transform_chain,
        transform_chain_parseable,
        modifier_chain: chain,
        modifier_chain_parseable: parseable,
        skin,
        physics,
        physics_imported,
        liquid_domain,
        fluid_controls,
        fluid_domain,
        fluid_domain_transform,
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
        node: node.map(|n| n.node_id.clone()).unwrap_or_default(),
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
/// no fabricated numbers. section 2.5 audit of every `Array(MeshVertex)`-producing
/// `Source`-role primitive: `node.cube_mesh` (a fixed 36-vertex
/// constant — 6 faces × 2 triangles × 3 vertices, `generate_cube_mesh.rs`),
/// `node.plane_mesh` (a fixed 6-vertex quad — 2 triangles × 3 vertices,
/// `plane_mesh.rs`), and `node.grid_mesh` (`resolution_x * resolution_y`,
/// confirmed against `generate_grid_mesh_body.wgsl`'s own index math) are
/// trivially closed-form; the rest (`node.revolve_curve`,
/// `node.extrude_curve`, `node.tube_from_path`,
/// `node.platonic_solid_points`, …) depend on curve length, topology
/// tables, or a dynamically wired selector — genuinely not computable from
/// static params alone, so they fall through to `None`.
fn procedural_vertex_count(node: &EffectGraphNode) -> Option<u32> {
    match node.type_id.as_str() {
        "node.cube_mesh" => Some(36),
        "node.plane_mesh" => Some(6),
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
                        node: node.node_id.clone(),
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
        let Some(node) = level.node(node_id) else {
            return CameraVm::None;
        };
        if node.type_id == CAMERA_LENS_TYPE_ID {
            lens_node_doc_id = Some(node.id);
            match level.producer(node.id, "camera") {
                Some((next, _)) => {
                    node_id = next;
                    continue;
                }
                None => {
                    return CameraVm::Custom {
                        node_doc_id: node.id,
                        lens: trace_lens(level, node.id),
                    };
                }
            }
        }
        break;
    }
    let Some(node) = level.node(node_id) else {
        return CameraVm::None;
    };
    let lens = lens_node_doc_id.and_then(|id| trace_lens(level, id));
    match node.type_id.as_str() {
        t if t == ORBIT_CAMERA_TYPE_ID => CameraVm::Orbit(Box::new(OrbitCameraRow {
            node_doc_id: node.id,
            lens,
        })),
        t if t == FREE_CAMERA_TYPE_ID => CameraVm::Free(Box::new(FreeCameraRow {
            node_doc_id: node.id,
            lens,
        })),
        t if t == LOOK_AT_CAMERA_TYPE_ID => CameraVm::LookAt(Box::new(LookAtCameraRow {
            node_doc_id: node.id,
            lens,
        })),
        t if t == LOOP_CAMERA_TYPE_ID => CameraVm::Loop(Box::new(LoopCameraRow {
            node_doc_id: node.id,
            lens,
        })),
        _ => CameraVm::Custom {
            node_doc_id: node.id,
            lens,
        },
    }
}

fn trace_environment(level: &Level, scene_node: &EffectGraphNode) -> EnvironmentVm {
    let Some((node_id, _)) = level.producer(scene_node.id, "envmap") else {
        return EnvironmentVm::None;
    };
    let Some(node) = level.node(node_id) else {
        return EnvironmentVm::None;
    };

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
    let Some(node) = level.node(node_id) else {
        return AtmosphereVm::None;
    };
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
    use manifold_core::liquid_domain::FLIP_DOMAIN_TYPE_ID;
    use std::collections::BTreeMap;

    /// Every node gets a stable id, as authoring and loading give them: the
    /// liquid walk resolves objects by stable path.
    fn node(id: u32, type_id: &str, handle: Option<&str>) -> EffectGraphNode {
        EffectGraphNode {
            id,
            node_id: manifold_core::NodeId::new(format!("n{id}")),
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
            scene_modifiers: Vec::new(),
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
    fn scene_physics_fluid_controls_follow_surface_and_shared_source() {
        let scene = with_param(node(1, RENDER_SCENE_TYPE_ID, None), "objects",
            SerializedParamValue::Float { value: 1.0 });
        let domain = with_param(
            with_param(
                with_param(node(6, "node.transform_3d", None), "pos_y",
                    SerializedParamValue::Float { value: 2.0 }),
                "scale_x", SerializedParamValue::Float { value: 4.0 }),
            "scale_y", SerializedParamValue::Float { value: 4.0 });
        let graph = def(vec![scene, node(2, "system.final_output", None),
            node(3, "node.scene_object", Some("Fluid")),
            node(4, FLIP_DOMAIN_TYPE_ID, None), node(5, "node.transform_3d", None), domain],
            vec![wire(1, "color", 2, "in"), wire(3, "out", 1, "object_0"),
                wire(6, "transform", 4, "domain"),
                wire(4, "vertices", 3, "vertices"), wire(5, "transform", 4, "emitter"),
                wire(5, "transform", 4, "initial_volume")]);
        let vm = SceneVm::from_def(&graph).unwrap();
        let SceneObjectVm::Known(row) = &vm.objects[0] else { panic!("fluid surface row"); };
        assert_eq!(row.fluid_controls, ["n4", "n6", "n5"].map(NodeId::new));
        let domain_transform = row.fluid_domain_transform.as_ref().expect("domain transform");
        assert_eq!(domain_transform.node_doc_id, 6);
        assert_eq!(domain_transform.pos_value, (0.0, 2.0, 0.0));
        assert_eq!(row.fluid_domain.expect("static domain").size, [4.0, 4.0, 1.0]);
        assert!(row.transform.is_none(), "source transform must not move only the visible mesh");
    }

    #[test]
    fn scene_physics_fluid_controls_own_the_whitewater_on_the_domain_clock() {
        let scene = with_param(node(1, RENDER_SCENE_TYPE_ID, None), "objects",
            SerializedParamValue::Float { value: 1.0 });
        let graph = def(vec![scene, node(2, "system.final_output", None),
            node(3, "node.scene_object", Some("Fluid")),
            node(4, manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID, None),
            node(7, "node.whitewater_step", None), node(8, "node.whitewater_step", None)],
            vec![wire(1, "color", 2, "in"), wire(3, "out", 1, "object_0"),
                wire(4, "vertices", 3, "vertices"), wire(4, "ticks", 7, "ticks")]);
        let vm = SceneVm::from_def(&graph).unwrap();
        let SceneObjectVm::Known(row) = &vm.objects[0] else { panic!("fluid surface row"); };
        assert_eq!(row.fluid_controls, ["n4", "n7"].map(NodeId::new), "n8 steps no clock of this water");
    }

    #[test]
    fn scene_physics_fluid_controls_own_values_wired_into_the_water() {
        let scene = with_param(node(1, RENDER_SCENE_TYPE_ID, None), "objects",
            SerializedParamValue::Float { value: 1.0 });
        let graph = def(vec![scene, node(2, "system.final_output", None),
            node(3, "node.scene_object", Some("Fluid")),
            node(4, manifold_core::liquid_domain::GPU_FLIP_DOMAIN_TYPE_ID, None),
            node(7, "node.whitewater_step", None), node(8, "node.whitewater_step", None),
            node(9, "node.value", None), node(10, "node.value", None)],
            vec![wire(1, "color", 2, "in"), wire(3, "out", 1, "object_0"),
                wire(4, "vertices", 3, "vertices"), wire(4, "epoch", 7, "epoch"),
                wire(9, "out", 7, "capacity"), wire(10, "out", 8, "capacity")]);
        let vm = SceneVm::from_def(&graph).unwrap();
        let SceneObjectVm::Known(row) = &vm.objects[0] else { panic!("fluid surface row"); };
        assert_eq!(row.fluid_controls, ["n4", "n7", "n9"].map(NodeId::new), "n10 feeds no node of this water");
    }

    #[test]
    fn scene_physics_fluid_roles_resolve_group_controls_and_deduplicate_transforms() {
        let scene = with_param(node(1, RENDER_SCENE_TYPE_ID, None), "objects",
            SerializedParamValue::Float { value: 1.0 });
        let mut source_group = node(10, GROUP_TYPE_ID, Some("Source"));
        source_group.group = Some(Box::new(GroupDef {
            interface: GroupInterface {
                inputs: vec![],
                outputs: vec![manifold_core::effect_graph_def::InterfacePortDef {
                    name: "role".into(),
                    port_type: "FluidRole".into(),
                }],
                params: vec![],
            },
            nodes: vec![node(11, "node.fluid_role_source", None),
                node(12, "node.transform_3d", None), node(13, GROUP_OUTPUT_TYPE_ID, None)],
            wires: vec![wire(12, "transform", 11, "transform"),
                wire(12, "transform", 11, "source_transform"), wire(11, "role", 13, "role")],
            tint: None,
        }));
        let graph = def(vec![scene, node(2, "system.final_output", None),
            node(3, "node.scene_object", Some("Fluid")),
            node(4, FLIP_DOMAIN_TYPE_ID, None), source_group],
            vec![wire(1, "color", 2, "in"), wire(3, "object", 1, "object_0"),
                wire(4, "vertices", 3, "vertices"), wire(10, "role", 4, "role_0"),
                wire(10, "role", 4, "role_1")]);
        let restored: EffectGraphDef = serde_json::from_str(&serde_json::to_string(&graph).unwrap()).unwrap();
        let vm = SceneVm::from_def(&restored).unwrap();
        let SceneObjectVm::Known(row) = &vm.objects[0] else { panic!("fluid surface row"); };
        assert_eq!(row.fluid_controls, ["n4", "n11", "n12"].map(NodeId::new));
        assert!(row.transform.is_none());
    }

    #[test]
    fn scene_physics_fluid_domain_rejects_non_transform_producer() {
        let scene = with_param(node(1, RENDER_SCENE_TYPE_ID, None), "objects",
            SerializedParamValue::Float { value: 1.0 });
        let graph = def(vec![scene, node(2, "system.final_output", None),
            node(3, SCENE_OBJECT_TYPE_ID, Some("Fluid")),
            node(4, FLIP_DOMAIN_TYPE_ID, None), node(6, "node.value", None)],
            vec![wire(1, "color", 2, "in"), wire(3, "out", 1, "object_0"),
                wire(4, "vertices", 3, "vertices"), wire(6, "out", 4, "domain")]);
        let vm = SceneVm::from_def(&graph).unwrap();
        let SceneObjectVm::Known(row) = &vm.objects[0] else { panic!("fluid surface row"); };
        assert!(row.fluid_domain.is_none());
        assert!(row.fluid_domain_transform.is_none());
    }

    #[test]
    fn scene_physics_fluid_domain_shadows_legacy_domain_size_driver() {
        let scene = with_param(node(1, RENDER_SCENE_TYPE_ID, None), "objects",
            SerializedParamValue::Float { value: 1.0 });
        let domain = with_param(
            with_param(
                with_param(node(6, TRANSFORM_3D_TYPE_ID, None), "pos_y",
                    SerializedParamValue::Float { value: 2.0 }),
            "scale_x", SerializedParamValue::Float { value: 4.0 }),
            "scale_y", SerializedParamValue::Float { value: 4.0 });
        let graph = def(vec![scene, node(2, "system.final_output", None),
            node(3, SCENE_OBJECT_TYPE_ID, Some("Fluid")),
            node(4, FLIP_DOMAIN_TYPE_ID, None), domain,
            node(7, "node.value", None)],
            vec![wire(1, "color", 2, "in"), wire(3, "out", 1, "object_0"),
                wire(4, "vertices", 3, "vertices"), wire(6, "transform", 4, "domain"),
                wire(7, "out", 4, "domain_size")]);
        let vm = SceneVm::from_def(&graph).unwrap();
        let SceneObjectVm::Known(row) = &vm.objects[0] else { panic!("fluid surface row"); };
        assert!(row.fluid_domain.is_some());
        assert_eq!(row.fluid_domain_transform.as_ref().unwrap().node_doc_id, 6);
    }

    #[test]
    fn scene_physics_fluid_domain_billboard_has_no_static_bounds() {
        let scene = with_param(node(1, RENDER_SCENE_TYPE_ID, None), "objects",
            SerializedParamValue::Float { value: 1.0 });
        let domain = with_param(node(6, TRANSFORM_3D_TYPE_ID, None), "billboard",
            SerializedParamValue::Bool { value: true });
        let graph = def(vec![scene, node(2, "system.final_output", None),
            node(3, SCENE_OBJECT_TYPE_ID, Some("Fluid")),
            node(4, FLIP_DOMAIN_TYPE_ID, None), domain],
            vec![wire(1, "color", 2, "in"), wire(3, "out", 1, "object_0"),
                wire(4, "vertices", 3, "vertices"), wire(6, "transform", 4, "domain")]);
        let vm = SceneVm::from_def(&graph).unwrap();
        let SceneObjectVm::Known(row) = &vm.objects[0] else { panic!("fluid surface row"); };
        assert!(row.fluid_domain.is_none());
        assert!(row.fluid_domain_transform.is_some());
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
        let mat = with_param(node(2, "node.cel_material", Some("mat")), "color_r", SerializedParamValue::Float { value: 0.4 });
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
                    MaterialVm::Known(m) => assert!(!m.is_pbr, "cel material is not PBR"),
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
            node(object_id + 102, "node.cel_material", Some("mat")),
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
                        assert!(!m.is_pbr, "cel material is not PBR");
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
            SceneObjectVm::Known(row) if row.group_node_id == Some(2) => {
                assert_eq!(row.name, "Grouped")
            }
            other => panic!("expected grouped Known object at index 0, got {other:?}"),
        }
        match &vm.objects[1] {
            SceneObjectVm::Known(row) if row.group_node_id.is_none() => {
                assert_eq!(row.name, "Bare")
            }
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
    fn pbr_material_gets_metallic_roughness_but_cel_does_not() {
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
                assert_eq!(m.texture_slots.len(), 19);
                assert!(m
                    .texture_slots
                    .iter()
                    .all(|slot| matches!(&slot.source, MaterialTextureSource::Unconnected)));
                assert_eq!(m.shared_object_count, Some(1));
            }
            other => panic!("expected Known pbr object, got {other:?}"),
        }
    }

    #[test]
    fn material_inspector_texture_facts_track_shared_identity_and_graph_sources() {
        let mesh0 = node(1, "node.cube_mesh", Some("mesh0"));
        let mesh1 = node(2, "node.cube_mesh", Some("mesh1"));
        let material = node(3, "node.pbr_material", Some("shared_material"));
        let object0 = node(4, SCENE_OBJECT_TYPE_ID, Some("Object 0"));
        let object1 = node(5, SCENE_OBJECT_TYPE_ID, Some("Object 1"));
        let map0 = node(6, "node.value", Some("map0"));
        let map1 = node(7, "node.value", Some("map1"));
        let scene = with_param(
            node(20, RENDER_SCENE_TYPE_ID, None),
            "objects",
            SerializedParamValue::Float { value: 2.0 },
        );
        let out = node(30, "system.final_output", None);
        let root = def(
            vec![mesh0, mesh1, material, object0, object1, map0, map1, scene, out],
            vec![
                wire(1, "vertices", 4, "vertices"),
                wire(2, "vertices", 5, "vertices"),
                wire(3, "out", 4, "material"),
                wire(3, "out", 5, "material"),
                wire(6, "out", 4, "base_color_map"),
                wire(7, "out", 5, "base_color_map"),
                wire(4, "object", 20, "object_0"),
                wire(5, "object", 20, "object_1"),
                wire(20, "color", 30, "in"),
            ],
        );
        let vm = SceneVm::from_def(&root).expect("scene must resolve");
        let material_rows: Vec<_> = vm
            .objects
            .iter()
            .filter_map(|object| match object {
                SceneObjectVm::Known(row) => match &row.material {
                    MaterialVm::Known(material) => Some(material),
                    MaterialVm::None => None,
                },
                SceneObjectVm::Custom { .. } => None,
            })
            .collect();
        assert_eq!(material_rows.len(), 2);
        assert!(material_rows.iter().all(|material| material.node_doc_id == 3));
        assert!(material_rows.iter().all(|material| material.shared_object_count == Some(2)));
        assert_eq!(material_rows[0].texture_slots.len(), 19);
        assert!(matches!(
            &material_rows[0]
                .texture_slots
                .iter()
                .find(|slot| slot.port == "base_color_map")
                .expect("base map slot")
                .source,
            MaterialTextureSource::Known { node_doc_id: 6, .. }
        ));
        assert!(matches!(
            &material_rows[1]
                .texture_slots
                .iter()
                .find(|slot| slot.port == "base_color_map")
                .expect("base map slot")
                .source,
            MaterialTextureSource::Known { node_doc_id: 7, .. }
        ));

        let mut unresolved = root.clone();
        unresolved.nodes.push(EffectGraphNode {
            group: Some(Box::new(GroupDef {
                interface: GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
                nodes: vec![],
                wires: vec![],
                tint: None,
            })),
            ..node(8, GROUP_TYPE_ID, Some("unresolved_map"))
        });
        unresolved.wires.retain(|wire| wire.from_node != 6);
        unresolved.wires.push(wire(8, "out", 4, "base_color_map"));
        let unresolved_vm = SceneVm::from_def(&unresolved).expect("scene must resolve");
        let SceneObjectVm::Known(row) = &unresolved_vm.objects[0] else { panic!("expected object") };
        let MaterialVm::Known(material) = &row.material else { panic!("expected material") };
        assert!(matches!(
            &material
                .texture_slots
                .iter()
                .find(|slot| slot.port == "base_color_map")
                .expect("base map slot")
                .source,
            MaterialTextureSource::GraphSource
        ));
        assert_eq!(material.shared_object_count, Some(2), "texture provenance does not change material users");
    }

    #[test]
    fn material_inspector_nested_sources_keep_object_scope() {
        let mut group = node(10, GROUP_TYPE_ID, Some("Nested Object"));
        group.group = Some(Box::new(GroupDef {
            interface: GroupInterface { inputs: vec![], outputs: vec![], params: vec![] },
            nodes: vec![
                node(1, "node.cube_mesh", Some("mesh")),
                node(2, "node.pbr_material", Some("material")),
                node(3, SCENE_OBJECT_TYPE_ID, Some("Object")),
                node(4, "node.value", Some("base_map")),
                node(5, GROUP_OUTPUT_TYPE_ID, Some("output")),
            ],
            wires: vec![
                wire(1, "vertices", 3, "vertices"),
                wire(2, "out", 3, "material"),
                wire(4, "out", 3, "base_color_map"),
                wire(3, "object", 5, "object"),
            ],
            tint: None,
        }));
        let scene = with_param(
            node(20, RENDER_SCENE_TYPE_ID, None),
            "objects",
            SerializedParamValue::Float { value: 1.0 },
        );
        let out = node(30, "system.final_output", None);
        let vm = SceneVm::from_def(&def(
            vec![group, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        ))
        .expect("nested scene must resolve");
        let SceneObjectVm::Known(row) = &vm.objects[0] else { panic!("expected object") };
        let MaterialVm::Known(material) = &row.material else { panic!("expected material") };
        assert_eq!(material.scope_path, vec![10]);
        assert_eq!(material.shared_object_count, Some(1));
        assert!(matches!(
            &material
                .texture_slots
                .iter()
                .find(|slot| slot.port == "base_color_map")
                .expect("base map slot")
                .source,
            MaterialTextureSource::Known { scope_path, node_doc_id: 4, .. } if scope_path == &vec![10]
        ));
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
        // from `manifold_core::project`. The negative `rg` gate (section 4) checks
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
        assert!(matches!(vm.camera, CameraVm::Custom { node_doc_id: 1, .. }));
    }

    #[test]
    fn camera_switch_preserves_shared_lens_and_tail_for_both_selections() {
        for select in [0, 1] {
            let mut d = importer_shaped_def();
            d.nodes.extend([
                with_param(node(30, "node.camera_switch", None), "select", SerializedParamValue::Enum { value: select }),
                node(31, LOOP_CAMERA_TYPE_ID, None),
                node(32, MOTION_BLUR_TYPE_ID, None),
                node(33, BOKEH_GATHER_TYPE_ID, None),
            ]);
            d.wires.retain(|w| !(w.to_node == 2 && w.to_port == "camera"));
            d.wires.extend([
                wire(1, "out", 30, "a"),
                wire(31, "out", 30, "b"),
                wire(30, "out", 2, "camera"),
            ]);
            let vm = SceneVm::from_def(&d).unwrap();
            let CameraVm::Custom { node_doc_id: 30, lens: Some(lens) } = vm.camera else {
                panic!("switch must retain shared lens: {:?}", vm.camera);
            };
            assert_eq!(lens.node_doc_id, 2);
            assert_eq!(vm.camera_controls, ["n2", "n32", "n33"].map(NodeId::new), "the lens and tail, never the switch");
        }
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
    /// the shipped, already-migrated bundled scene preset (Scene and
    /// the ~9 others P2 regenerated) silently showed no transform/material
    /// controls and a wrong vertex count in the panel, despite rendering
    /// correctly (the render path reads through `SceneObject`'s resolved Slots,
    /// never through this trace).
    #[test]
    fn bundled_scene_starter_preset_resolves_transform_material_and_vertex_count() {
        let preset_type = manifold_core::PresetTypeId::from_string("Scene".to_string());
        let d = crate::node_graph::bundled_presets::bundled_preset_def(&preset_type)
            .expect("Scene is a bundled preset");
        let vm = SceneVm::from_def(d).expect("Scene resolves");
        assert_eq!(vm.objects.len(), 1, "Cube");
        for obj in &vm.objects {
            let SceneObjectVm::Known(row) = obj else {
                panic!("Scene's objects must resolve Known, not Custom — migration shape unparsed");
            };
            assert!(row.transform.is_some(), "{}: transform must resolve through the group boundary", row.name);
            assert!(
                !matches!(row.material, MaterialVm::None),
                "{}: material must resolve through the group boundary",
                row.name
            );
        }
        assert!(vm.header.vertex_count > 0, "vertex count must resolve through the group boundary, not silently 0");
        assert!(vm.header.vertex_count_exact, "Scene's mesh sources have known vertex counts");
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

    /// P3: the transform wire can carry `node.transform_shake` between the
    /// `node.transform_3d` source and `node.scene_object.transform`. The
    /// walk resolves the underlying transform_3d and records the modifier.
    #[test]
    fn transform_chain_captures_transform_shake_before_scene_object() {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let transform = node(2, TRANSFORM_3D_TYPE_ID, Some("transform"));
        let shake = node(3, "node.transform_shake", Some("shake"));
        let scene_obj = node(4, SCENE_OBJECT_TYPE_ID, Some("Obj"));
        let gout = node(5, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Obj"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, transform, shake, scene_obj, gout],
            wires: vec![
                wire(1, "vertices", 4, "vertices"),
                wire(2, "transform", 3, "transform"),
                wire(3, "out", 4, "transform"),
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
                assert!(row.transform.is_some(), "transform_3d resolves through the shake atom");
                assert_eq!(row.transform.as_ref().unwrap().node_doc_id, 2);
                assert!(row.transform_chain_parseable);
                assert_eq!(row.transform_chain.len(), 1);
                assert_eq!(row.transform_chain[0].type_id, "node.transform_shake");
                assert_eq!(row.transform_chain[0].node_doc_id, 3);
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    /// P3: a genuinely unparseable transform wire (no transform_3d at the
    /// source end) degrades gracefully — `transform` is None but the walk
    /// still reports it as unparseable.
    #[test]
    fn transform_chain_without_transform_3d_is_unparseable() {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let shake = node(3, "node.transform_shake", Some("shake"));
        let scene_obj = node(4, SCENE_OBJECT_TYPE_ID, Some("Obj"));
        let gout = node(5, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Obj"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, shake, scene_obj, gout],
            wires: vec![
                wire(1, "vertices", 4, "vertices"),
                wire(3, "out", 4, "transform"),
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
                assert!(row.transform.is_none(), "no transform_3d source — transform row absent");
                assert!(!row.transform_chain_parseable, "chain without a transform_3d source is unparseable");
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    /// BUG-f3qd: a scene_object whose `base_color_map` is fed by a
    /// `node.layer_source` pointing at a deleted/nonexistent layer id
    /// yields a `SkinVm` with `source_missing == true` and
    /// `target_map == BaseColor`. Proves the same discovery path already
    /// used for emissive skins also covers the layer-plane `base_color_map`
    /// shape.
    #[test]
    fn base_color_skin_with_missing_layer_id_flags_source_missing() {
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(1, "node.plane_mesh", Some("mesh"));
        let mat = node(2, "node.unlit_material", Some("mat"));
        let transform = node(3, TRANSFORM_3D_TYPE_ID, Some("transform"));
        let skin = with_param(
            node(4, "node.layer_source", Some("Skin")),
            "layer",
            SerializedParamValue::String { value: "deleted-layer".to_string() },
        );
        let scene_obj = node(5, SCENE_OBJECT_TYPE_ID, Some("Plane"));
        let gout = node(6, GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, GROUP_TYPE_ID, Some("Layer Plane 1"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, mat, transform, skin, scene_obj, gout],
            wires: vec![
                wire(1, "vertices", 5, "vertices"),
                wire(2, "out", 5, "material"),
                wire(3, "transform", 5, "transform"),
                wire(4, "out", 5, "base_color_map"),
                wire(5, "object", 6, "object"),
            ],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "object", 20, "object_0"), wire(20, "color", 30, "in")],
        );
        let existing_layer = LayerId::new("existing-layer");
        let vm = SceneVm::from_def_with_layers(&d, &[existing_layer]).expect("scene resolves");
        assert_eq!(vm.objects.len(), 1);
        match &vm.objects[0] {
            SceneObjectVm::Known(row) => {
                let skin = row.skin.as_ref().expect("base_color_map skin is discovered");
                assert_eq!(skin.source_node_id, 4);
                assert_eq!(skin.source_layer_id.as_deref(), Some("deleted-layer"));
                assert_eq!(skin.target_map, SkinTargetMap::BaseColor);
                assert!(skin.source_missing, "a layer id not in the project layer list is flagged missing");
            }
            other => panic!("expected Known object, got {other:?}"),
        }
    }

    /// The GPU liquid's water is found by the same walk forces use, through
    /// its Liquid Surface group, so its object carries the domain's panel.
    #[test]
    fn scene_vm_traces_matter_domain() {
        let def: EffectGraphDef = serde_json::from_str(include_str!(
            "../../assets/generator-presets/WaterDamBreakMatter.json"
        ))
        .expect("preset parses");
        let vm = SceneVm::from_def(&def).expect("scene resolves");
        let domain = def
            .nodes
            .iter()
            .filter_map(|group| Some((group, group.group.as_deref()?)))
            .find_map(|(group, body)| {
                let node = body.nodes.iter().find(|node| {
                    node.type_id == manifold_core::liquid_domain::MATTER_DOMAIN_TYPE_ID
                })?;
                Some(SceneNodeRef { scope: vec![group.node_id.clone()], node: node.node_id.clone() })
            })
            .expect("the preset has a matter domain");
        let water: Vec<_> = vm
            .objects
            .iter()
            .filter_map(|row| match row {
                SceneObjectVm::Known(row) if row.liquid_domain.is_some() => Some(row),
                _ => None,
            })
            .collect();
        assert_eq!(water.len(), 1, "one water object");
        assert_eq!(water[0].liquid_domain.as_ref(), Some(&domain));
        assert_eq!(water[0].fluid_controls.first(), Some(&domain.node));
        // The domain shares its group-local doc id with the root camera.
        let camera = def.nodes.iter().find(|node| node.type_id == ORBIT_CAMERA_TYPE_ID).unwrap();
        assert!(!water[0].fluid_controls.contains(&camera.node_id));
        assert_eq!(vm.camera_controls.first(), Some(&camera.node_id));
        assert!(!vm.camera_controls.contains(&domain.node));
    }

    #[test]
    fn water_family_row_ownership() {
        for preset in ["WaterDamBreakGpuFlip", "WaterDamBreakParticles"] {
            let preset_type = manifold_core::PresetTypeId::new(preset);
            let def = crate::node_graph::bundled_presets::bundled_preset_def(&preset_type)
                .expect("water family preset");
            let vm = SceneVm::from_def(def).expect("water family scene resolves");
            let family: Vec<_> = vm.objects.iter().filter_map(|object| match object {
                SceneObjectVm::Known(row)
                    if row.name == "Water" || row.parent_group_id.is_some() => Some(row),
                _ => None,
            }).collect();
            assert_eq!(family.iter().filter(|row| row.name == "Water").count(), 1, "{preset}");
            let water = family.iter().find(|row| row.name == "Water").expect("Water row");
            assert!(water.is_group, "{preset}: Water is the physical family parent");
            assert!(water.parent_group_id.is_none());
            assert!(water.group_node_id.is_some());
            assert!(water.look_mesh.is_none());
            assert_eq!(water.visible_addr.param_id, "parent_visible");
            assert!(water.liquid_domain.is_some());
            assert!(!water.fluid_controls.is_empty());

            let names: Vec<_> = family.iter().map(|row| row.name.as_str()).collect();
            assert_eq!(names, ["Water", "Foam", "Spray", "Bubbles"], "{preset}: family order");
            for row in family.iter().filter(|row| row.name != "Water") {
                assert_eq!(row.parent_group_id, Some(water.object_node_id), "{preset}: {} parent", row.name);
                assert!(row.look_mesh.is_some(), "{preset}: {} owns its platonic mesh", row.name);
                assert!(row.liquid_domain.is_none(), "{preset}: {} has no fluid domain", row.name);
                assert!(row.fluid_controls.is_empty(), "{preset}: {} has no fluid controls", row.name);
                assert!(row.fluid_domain.is_none());
                assert!(row.fluid_domain_transform.is_none());
                assert!(row.transform.is_none());
                assert!(row.transform_chain.is_empty());
                assert!(row.modifier_chain.is_empty());
                assert!(row.physics.is_none());
                assert!(row.skin.is_none());
            }
        }
    }
}
