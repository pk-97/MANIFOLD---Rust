//! `SceneVm` — the Scene Setup Panel's sole discovery mechanism
//! (`docs/SCENE_SETUP_PANEL_DESIGN.md` D3).
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
//! chain, `node.light`, the three camera atoms, `node.atmosphere`) get
//! editable rows; anything else degrades to an honest labeled "custom"
//! row. Nothing is ever hidden and nothing errors.
//!
//! Rebuilt from scratch on every `state_sync` pass — no cached/staged
//! copy anywhere (Peter: "no rotting, no staleness"). See the D3 "plausible
//! wrong architecture" callout: this module must never grow a persistent
//! mirror of scene values.

use std::collections::HashSet;

use manifold_core::effect_graph_def::{EffectGraphDef, EffectGraphNode, SerializedParamValue};

use crate::node_graph::FINAL_OUTPUT_TYPE_ID;

/// `node.render_scene`'s own type_id string (curated vocabulary anchor).
pub const RENDER_SCENE_TYPE_ID: &str = "node.render_scene";
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

impl ParamAddr {
    fn root(node_doc_id: u32, param_id: &str) -> Self {
        Self { scope_path: Vec::new(), node_doc_id, param_id: param_id.to_string() }
    }
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
    /// Sum of resolved objects' vertex-source node count — a cheap proxy;
    /// the panel shows it as an honest "counts, not cost" line (§9 Deferred
    /// owns real per-scene ms attribution).
    pub shadow_caster_count: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SceneObjectVm {
    /// Producer resolved to a named group (SCENE_BUILD D9 shape).
    Known {
        index: usize,
        /// The object's own group node doc id — the address `RenameGroupCommand`
        /// and any future group-scoped composite (splice, remove) takes.
        group_node_id: u32,
        name: String,
        tint: Option<[f32; 4]>,
        transform: Option<TransformVm>,
        material: MaterialVm,
        /// Chain of single-mesh-input/mesh-output nodes between the mesh
        /// source and the group output, in wire order (D6's modifier stack).
        modifier_chain: Vec<ModifierVm>,
    },
    /// Producer did NOT resolve to a group output — "Object k — custom
    /// (edit in graph)" per D3, degraded but never hidden.
    Custom { index: usize, transform: Option<TransformVm> },
}

#[derive(Debug, Clone, PartialEq)]
pub struct ModifierVm {
    pub node_doc_id: u32,
    pub type_id: String,
}

/// One `node.transform_3d`'s write addresses + current values — D4's "3
/// compact triplets" (Position/Rotation/Scale), each X/Y/Z. Traced
/// independently of the object's group (SCENE_BUILD D9 places it in the same
/// group by convention, but the trace never assumes that — a hand-wired
/// `transform_k` still resolves here as long as the producer IS a
/// `node.transform_3d`).
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
    pub type_id: String,
    pub base_color_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub base_color_value: (f32, f32, f32),
    pub base_color_driven: (bool, bool, bool),
    /// `Some` only for `node.pbr_material` — metallic/roughness is a PBR-only
    /// concept, so a phong/unlit/cel material's quick knobs are base color
    /// alone (D4: "the atom's own params otherwise").
    pub metallic_roughness: Option<MetallicRoughnessRow>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MetallicRoughnessRow {
    pub metallic_addr: ParamAddr,
    pub metallic_value: f32,
    pub metallic_driven: bool,
    pub roughness_addr: ParamAddr,
    pub roughness_value: f32,
    pub roughness_driven: bool,
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
    pub mode_addr: ParamAddr,
    pub mode_value: u32,
    pub mode_driven: bool,
    pub color_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub color_value: (f32, f32, f32),
    pub color_driven: (bool, bool, bool),
    pub intensity_addr: ParamAddr,
    pub intensity_value: f32,
    pub intensity_driven: bool,
    pub pos_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub pos_value: (f32, f32, f32),
    pub pos_driven: (bool, bool, bool),
    pub aim_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub aim_value: (f32, f32, f32),
    pub aim_driven: (bool, bool, bool),
    pub cast_shadows_addr: ParamAddr,
    /// `true` when the raw [0,1] threshold is > 0.5 (`node.light`'s own
    /// on/off convention, REALTIME_3D D4).
    pub cast_shadows_value: bool,
    pub cast_shadows_driven: bool,
    pub shadow_softness_addr: ParamAddr,
    pub shadow_softness_value: u32,
    pub shadow_softness_driven: bool,
    pub light_size_addr: ParamAddr,
    pub light_size_value: f32,
    pub light_size_driven: bool,
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
    pub focus_distance_addr: ParamAddr,
    pub focus_distance_value: f32,
    pub focus_distance_driven: bool,
    pub f_stop_addr: ParamAddr,
    pub f_stop_value: f32,
    pub f_stop_driven: bool,
    pub shutter_angle_addr: ParamAddr,
    pub shutter_angle_value: f32,
    pub shutter_angle_driven: bool,
    pub exposure_ev_addr: ParamAddr,
    pub exposure_ev_value: f32,
    pub exposure_ev_driven: bool,
}

/// Payload for [`CameraVm::Orbit`], boxed for the same reason as
/// [`LightRow`].
#[derive(Debug, Clone, PartialEq)]
pub struct OrbitCameraRow {
    pub node_doc_id: u32,
    pub lens: Option<LensRow>,
    pub orbit_addr: ParamAddr,
    pub orbit_value: f32,
    pub orbit_driven: bool,
    pub tilt_addr: ParamAddr,
    pub tilt_value: f32,
    pub tilt_driven: bool,
    pub distance_addr: ParamAddr,
    pub distance_value: f32,
    pub distance_driven: bool,
    pub fov_y_addr: ParamAddr,
    pub fov_y_value: f32,
    pub fov_y_driven: bool,
}

/// Payload for [`CameraVm::Free`] (D3: "free: pos/euler/fov rows").
#[derive(Debug, Clone, PartialEq)]
pub struct FreeCameraRow {
    pub node_doc_id: u32,
    pub lens: Option<LensRow>,
    pub pos_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub pos_value: (f32, f32, f32),
    pub pos_driven: (bool, bool, bool),
    pub yaw_addr: ParamAddr,
    pub yaw_value: f32,
    pub yaw_driven: bool,
    pub pitch_addr: ParamAddr,
    pub pitch_value: f32,
    pub pitch_driven: bool,
    pub roll_addr: ParamAddr,
    pub roll_value: f32,
    pub roll_driven: bool,
    pub fov_y_addr: ParamAddr,
    pub fov_y_value: f32,
    pub fov_y_driven: bool,
}

/// Payload for [`CameraVm::LookAt`] (D3: "look-at: pos/target/fov rows").
#[derive(Debug, Clone, PartialEq)]
pub struct LookAtCameraRow {
    pub node_doc_id: u32,
    pub lens: Option<LensRow>,
    pub pos_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub pos_value: (f32, f32, f32),
    pub pos_driven: (bool, bool, bool),
    pub target_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub target_value: (f32, f32, f32),
    pub target_driven: (bool, bool, bool),
    pub fov_y_addr: ParamAddr,
    pub fov_y_value: f32,
    pub fov_y_driven: bool,
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
    pub mode_addr: ParamAddr,
    pub mode_value: u32,
    pub intensity_addr: ParamAddr,
    pub intensity_value: f32,
    /// `true` when a wire feeds `intensity` directly (the primitive's
    /// port-shadow convention: an input port sharing the param's name) — the
    /// panel renders the row read-only with the same "driven" styling the
    /// group-face rows use (D4), never fighting the graph.
    pub intensity_driven: bool,
    pub fill_addr: ParamAddr,
    pub fill_value: f32,
    pub fill_driven: bool,
    pub hdri_file_addr: ParamAddr,
    pub hdri_file_value: String,
}

/// Payload for [`EnvironmentVm::Bare`].
#[derive(Debug, Clone, PartialEq)]
pub struct BareEnvironmentRow {
    pub intensity_addr: ParamAddr,
    pub intensity_value: f32,
    pub intensity_driven: bool,
    pub fill_addr: ParamAddr,
    pub fill_value: f32,
    pub fill_driven: bool,
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
    pub density_addr: ParamAddr,
    pub density_value: f32,
    pub density_driven: bool,
    pub color_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub color_value: (f32, f32, f32),
    pub height_falloff_addr: ParamAddr,
    pub height_falloff_value: f32,
    pub height_falloff_driven: bool,
    pub ambient_tint_addr: (ParamAddr, ParamAddr, ParamAddr),
    pub ambient_tint_value: (f32, f32, f32),
}

#[derive(Debug, Clone, PartialEq)]
pub enum AtmosphereVm {
    Wired(Box<AtmosphereRow>),
    /// `atmosphere` unwired — D3's "Add fog" action.
    None,
}

/// Minimal view of one node + its incoming wires, scoped to a single graph
/// level (root or inside a group) — the trace never crosses a group
/// boundary itself (SCENE_BUILD's named-group identity is the object's
/// resolution unit).
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

        let objects = trace_objects(&root, scene_node);
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
            header: SceneHeaderVm { object_count, light_count, shadow_caster_count },
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

fn trace_objects(level: &Level, scene_node: &EffectGraphNode) -> Vec<SceneObjectVm> {
    let objects = param_f32(scene_node, "objects", 0.0).max(0.0) as usize;
    (0..objects)
        .map(|k| {
            let mesh_port = format!("mesh_{k}");

            match level.producer(scene_node.id, &mesh_port) {
                Some((producer_id, _port)) => {
                    let Some(producer_node) = level.node(producer_id) else {
                        // No node at all for the mesh producer id (a
                        // malformed def) — the root-level `transform_k` is
                        // the only place left to look (D3's Custom case).
                        let transform = trace_root_transform(level, scene_node.id, k);
                        return SceneObjectVm::Custom { index: k, transform };
                    };
                    if producer_node.type_id != manifold_core::effect_graph_def::GROUP_TYPE_ID {
                        let transform = trace_root_transform(level, scene_node.id, k);
                        return SceneObjectVm::Custom { index: k, transform };
                    }
                    let name = producer_node
                        .handle
                        .clone()
                        .unwrap_or_else(|| format!("Object {k}"));
                    let tint = producer_node.group.as_ref().and_then(|g| g.tint);

                    // Modifier chain + material + transform: walk the
                    // group's INNER wires (a nested `Level`) — the shipped
                    // shape (`AddSceneObjectCommand`, the glTF importer) puts
                    // `node.transform_3d` INSIDE the object's group, passing
                    // its `transform` output through the group's own
                    // `transform` interface port to the root `transform_k`
                    // wire; the root-level wire's producer is the GROUP, not
                    // the transform atom itself, so the transform can only
                    // be found by looking inside (SCENE_BUILD D9). Best-
                    // effort: an unparseable chain still shows the group as
                    // Known with an empty modifier list (nothing errors,
                    // per D3).
                    let (modifier_chain, material, transform) = producer_node
                        .group
                        .as_ref()
                        .map(|g| trace_group_body(producer_id, g))
                        .unwrap_or((Vec::new(), MaterialVm::None, None));

                    SceneObjectVm::Known {
                        index: k,
                        group_node_id: producer_id,
                        name,
                        tint,
                        transform,
                        material,
                        modifier_chain,
                    }
                }
                None => {
                    let transform = trace_root_transform(level, scene_node.id, k);
                    SceneObjectVm::Custom { index: k, transform }
                }
            }
        })
        .collect()
}

/// D3's Custom-row transform fallback: `transform_k` traced at the SAME
/// level as `scene_node` (root), expecting a bare `node.transform_3d`
/// directly — the shape a hand-built/custom object might use when it isn't
/// wrapped in a group at all. Never consulted for a Known (group) object —
/// see `trace_group_body`'s transform trace for that shape.
fn trace_root_transform(level: &Level, scene_node_id: u32, k: usize) -> Option<TransformVm> {
    level
        .producer(scene_node_id, &format!("transform_{k}"))
        .filter(|(n, _)| level.node(*n).is_some_and(|nn| nn.type_id == TRANSFORM_3D_TYPE_ID))
        .map(|(n, _)| trace_transform(level, Vec::new(), n))
}

/// Traces one `node.transform_3d`'s nine params at `level` into a full
/// [`TransformVm`], addressed with `scope_path` — empty for the D3 Custom
/// root-level fallback, `[group_node_id]` when the atom lives inside an
/// object's group (the shipped shape).
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

/// Trace one object group's body: the modifier chain feeding the group
/// output's `vertices` port, the material feeding `material`, and the
/// transform feeding `transform` — all three atoms live INSIDE the group
/// (SCENE_BUILD D9 / `AddSceneObjectCommand` / the glTF importer all wire it
/// this way), so every write address here carries `scope_path =
/// [group_node_id]`, the exact scope `SetGraphNodeParamCommand::with_scope`
/// needs to reach a node nested one level down.
fn trace_group_body(
    group_node_id: u32,
    group: &manifold_core::effect_graph_def::GroupDef,
) -> (Vec<ModifierVm>, MaterialVm, Option<TransformVm>) {
    use manifold_core::effect_graph_def::GROUP_OUTPUT_TYPE_ID;
    let inner = Level { nodes: &group.nodes, wires: &group.wires };
    let scope = vec![group_node_id];
    let Some(out_node) = inner.nodes.iter().find(|n| n.type_id == GROUP_OUTPUT_TYPE_ID) else {
        return (Vec::new(), MaterialVm::None, None);
    };

    let mut chain = Vec::new();
    let mut cursor = inner.producer(out_node.id, "vertices");
    let mut guard = 0;
    while let Some((node_id, _port)) = cursor {
        guard += 1;
        if guard > 64 {
            break; // cycle guard — never hang the panel on malformed JSON.
        }
        let Some(node) = inner.node(node_id) else { break };
        if !MODIFIER_TYPE_IDS.contains(&node.type_id.as_str()) {
            break; // reached the mesh source (or something un-curated) — stop.
        }
        chain.push(ModifierVm { node_doc_id: node.id, type_id: node.type_id.clone() });
        cursor = inner.producer(node.id, "in");
    }
    chain.reverse(); // wire order: source → … → output.

    let material = inner
        .producer(out_node.id, "material")
        .and_then(|(n, _)| inner.node(n))
        .filter(|n| MATERIAL_TYPE_IDS.contains(&n.type_id.as_str()))
        .map(|n| {
            let driven = |name: &str| inner.producer(n.id, name).is_some();
            let addr = |name: &str| ParamAddr { scope_path: scope.clone(), node_doc_id: n.id, param_id: name.to_string() };
            let metallic_roughness = (n.type_id == "node.pbr_material").then(|| MetallicRoughnessRow {
                metallic_addr: addr("metallic"),
                metallic_value: param_f32(n, "metallic", 0.0),
                metallic_driven: driven("metallic"),
                roughness_addr: addr("roughness"),
                roughness_value: param_f32(n, "roughness", 0.5),
                roughness_driven: driven("roughness"),
            });
            MaterialVm::Known(Box::new(MaterialColorRow {
                node_doc_id: n.id,
                type_id: n.type_id.clone(),
                base_color_addr: (addr("color_r"), addr("color_g"), addr("color_b")),
                base_color_value: (
                    param_f32(n, "color_r", 0.8),
                    param_f32(n, "color_g", 0.8),
                    param_f32(n, "color_b", 0.8),
                ),
                base_color_driven: (driven("color_r"), driven("color_g"), driven("color_b")),
                metallic_roughness,
            }))
        })
        .unwrap_or(MaterialVm::None);

    let transform = inner
        .producer(out_node.id, "transform")
        .filter(|(n, _)| inner.node(*n).is_some_and(|nn| nn.type_id == TRANSFORM_3D_TYPE_ID))
        .map(|(n, _)| trace_transform(&inner, scope.clone(), n));

    (chain, material, transform)
}

fn trace_lights(level: &Level, scene_node: &EffectGraphNode) -> Vec<SceneLightVm> {
    let lights = param_f32(scene_node, "lights", 0.0).max(0.0) as usize;
    (0..lights)
        .map(|k| {
            let port = format!("light_{k}");
            match level.producer(scene_node.id, &port) {
                Some((node_id, _)) if level.node(node_id).is_some_and(|n| n.type_id == LIGHT_TYPE_ID) => {
                    let node = level.node(node_id).expect("checked above");
                    let driven = |name: &str| level.producer(node_id, name).is_some();
                    SceneLightVm::Known(Box::new(LightRow {
                        index: k,
                        node_doc_id: node_id,
                        mode_addr: ParamAddr::root(node_id, "mode"),
                        mode_value: param_f32(node, "mode", 0.0) as u32,
                        mode_driven: driven("mode"),
                        color_addr: (
                            ParamAddr::root(node_id, "color_r"),
                            ParamAddr::root(node_id, "color_g"),
                            ParamAddr::root(node_id, "color_b"),
                        ),
                        color_value: (
                            param_f32(node, "color_r", 1.0),
                            param_f32(node, "color_g", 1.0),
                            param_f32(node, "color_b", 1.0),
                        ),
                        color_driven: (driven("color_r"), driven("color_g"), driven("color_b")),
                        intensity_addr: ParamAddr::root(node_id, "intensity"),
                        intensity_value: param_f32(node, "intensity", 1.0),
                        intensity_driven: driven("intensity"),
                        pos_addr: (
                            ParamAddr::root(node_id, "pos_x"),
                            ParamAddr::root(node_id, "pos_y"),
                            ParamAddr::root(node_id, "pos_z"),
                        ),
                        pos_value: (
                            param_f32(node, "pos_x", 0.0),
                            param_f32(node, "pos_y", 30.0),
                            param_f32(node, "pos_z", 0.0),
                        ),
                        pos_driven: (driven("pos_x"), driven("pos_y"), driven("pos_z")),
                        aim_addr: (
                            ParamAddr::root(node_id, "aim_x"),
                            ParamAddr::root(node_id, "aim_y"),
                            ParamAddr::root(node_id, "aim_z"),
                        ),
                        aim_value: (
                            param_f32(node, "aim_x", 0.0),
                            param_f32(node, "aim_y", 0.0),
                            param_f32(node, "aim_z", 0.0),
                        ),
                        aim_driven: (driven("aim_x"), driven("aim_y"), driven("aim_z")),
                        cast_shadows_addr: ParamAddr::root(node_id, "cast_shadows"),
                        cast_shadows_value: param_f32(node, "cast_shadows", 1.0) > 0.5,
                        cast_shadows_driven: driven("cast_shadows"),
                        shadow_softness_addr: ParamAddr::root(node_id, "shadow_softness"),
                        shadow_softness_value: param_f32(node, "shadow_softness", 1.0) as u32,
                        shadow_softness_driven: driven("shadow_softness"),
                        light_size_addr: ParamAddr::root(node_id, "light_size"),
                        light_size_value: param_f32(node, "light_size", 1.0),
                        light_size_driven: driven("light_size"),
                    }))
                }
                _ => SceneLightVm::Custom { index: k },
            }
        })
        .collect()
}

/// Builds a [`LensRow`] for `node.camera_lens` at `node_id` — its four
/// port-shadowed scalar params, addressed and valued (D3's "the lens node's
/// own row beneath").
fn trace_lens(level: &Level, node_id: u32) -> Option<LensRow> {
    let node = level.node(node_id)?;
    let driven = |name: &str| level.producer(node_id, name).is_some();
    Some(LensRow {
        node_doc_id: node_id,
        focus_distance_addr: ParamAddr::root(node_id, "focus_distance"),
        focus_distance_value: param_f32(node, "focus_distance", 0.0),
        focus_distance_driven: driven("focus_distance"),
        f_stop_addr: ParamAddr::root(node_id, "f_stop"),
        f_stop_value: param_f32(node, "f_stop", 1000.0),
        f_stop_driven: driven("f_stop"),
        shutter_angle_addr: ParamAddr::root(node_id, "shutter_angle"),
        shutter_angle_value: param_f32(node, "shutter_angle", 0.0),
        shutter_angle_driven: driven("shutter_angle"),
        exposure_ev_addr: ParamAddr::root(node_id, "exposure_ev"),
        exposure_ev_value: param_f32(node, "exposure_ev", 0.0),
        exposure_ev_driven: driven("exposure_ev"),
    })
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
    let driven = |name: &str| level.producer(node.id, name).is_some();
    match node.type_id.as_str() {
        t if t == ORBIT_CAMERA_TYPE_ID => CameraVm::Orbit(Box::new(OrbitCameraRow {
            node_doc_id: node.id,
            lens,
            orbit_addr: ParamAddr::root(node.id, "orbit"),
            orbit_value: param_f32(node, "orbit", 0.7),
            orbit_driven: driven("orbit"),
            tilt_addr: ParamAddr::root(node.id, "tilt"),
            tilt_value: param_f32(node, "tilt", 0.3),
            tilt_driven: driven("tilt"),
            distance_addr: ParamAddr::root(node.id, "distance"),
            distance_value: param_f32(node, "distance", 4.0),
            distance_driven: driven("distance"),
            fov_y_addr: ParamAddr::root(node.id, "fov_y"),
            fov_y_value: param_f32(node, "fov_y", 0.9),
            fov_y_driven: driven("fov_y"),
        })),
        t if t == FREE_CAMERA_TYPE_ID => CameraVm::Free(Box::new(FreeCameraRow {
            node_doc_id: node.id,
            lens,
            pos_addr: (
                ParamAddr::root(node.id, "pos_x"),
                ParamAddr::root(node.id, "pos_y"),
                ParamAddr::root(node.id, "pos_z"),
            ),
            pos_value: (
                param_f32(node, "pos_x", 0.0),
                param_f32(node, "pos_y", 0.0),
                param_f32(node, "pos_z", -3.0),
            ),
            pos_driven: (driven("pos_x"), driven("pos_y"), driven("pos_z")),
            yaw_addr: ParamAddr::root(node.id, "yaw"),
            yaw_value: param_f32(node, "yaw", 0.0),
            yaw_driven: driven("yaw"),
            pitch_addr: ParamAddr::root(node.id, "pitch"),
            pitch_value: param_f32(node, "pitch", 0.0),
            pitch_driven: driven("pitch"),
            roll_addr: ParamAddr::root(node.id, "roll"),
            roll_value: param_f32(node, "roll", 0.0),
            roll_driven: driven("roll"),
            fov_y_addr: ParamAddr::root(node.id, "fov_y"),
            fov_y_value: param_f32(node, "fov_y", 0.9),
            fov_y_driven: driven("fov_y"),
        })),
        t if t == LOOK_AT_CAMERA_TYPE_ID => CameraVm::LookAt(Box::new(LookAtCameraRow {
            node_doc_id: node.id,
            lens,
            pos_addr: (
                ParamAddr::root(node.id, "pos_x"),
                ParamAddr::root(node.id, "pos_y"),
                ParamAddr::root(node.id, "pos_z"),
            ),
            pos_value: (
                param_f32(node, "pos_x", 0.0),
                param_f32(node, "pos_y", 0.0),
                param_f32(node, "pos_z", -3.0),
            ),
            pos_driven: (driven("pos_x"), driven("pos_y"), driven("pos_z")),
            target_addr: (
                ParamAddr::root(node.id, "target_x"),
                ParamAddr::root(node.id, "target_y"),
                ParamAddr::root(node.id, "target_z"),
            ),
            target_value: (
                param_f32(node, "target_x", 0.0),
                param_f32(node, "target_y", 0.0),
                param_f32(node, "target_z", 0.0),
            ),
            target_driven: (driven("target_x"), driven("target_y"), driven("target_z")),
            fov_y_addr: ParamAddr::root(node.id, "fov_y"),
            fov_y_value: param_f32(node, "fov_y", 0.9),
            fov_y_driven: driven("fov_y"),
        })),
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
                mode_addr: ParamAddr::root(node.id, "selector"),
                mode_value: param_f32(node, "selector", 0.0) as u32,
                intensity_addr: ParamAddr::root(bake.id, "intensity"),
                intensity_value: param_f32(bake, "intensity", 1.0),
                intensity_driven: level.producer(bake.id, "intensity").is_some(),
                fill_addr: ParamAddr::root(bake.id, "fill"),
                fill_value: param_f32(bake, "fill", 0.0),
                fill_driven: level.producer(bake.id, "fill").is_some(),
                hdri_file_addr: ParamAddr::root(hdri_id, "path"),
                hdri_file_value,
            }));
        }
        return EnvironmentVm::Custom { node_doc_id: node.id };
    }

    if node.type_id == BAKE_ENVIRONMENT_TYPE_ID {
        return EnvironmentVm::Bare(Box::new(BareEnvironmentRow {
            intensity_addr: ParamAddr::root(node.id, "intensity"),
            intensity_value: param_f32(node, "intensity", 1.0),
            intensity_driven: level.producer(node.id, "intensity").is_some(),
            fill_addr: ParamAddr::root(node.id, "fill"),
            fill_value: param_f32(node, "fill", 0.0),
            fill_driven: level.producer(node.id, "fill").is_some(),
        }));
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
    AtmosphereVm::Wired(Box::new(AtmosphereRow {
        node_doc_id: node.id,
        density_addr: ParamAddr::root(node.id, "fog_density"),
        density_value: param_f32(node, "fog_density", 0.0),
        density_driven: level.producer(node.id, "fog_density").is_some(),
        color_addr: (
            ParamAddr::root(node.id, "fog_color_r"),
            ParamAddr::root(node.id, "fog_color_g"),
            ParamAddr::root(node.id, "fog_color_b"),
        ),
        color_value: (
            param_f32(node, "fog_color_r", 0.5),
            param_f32(node, "fog_color_g", 0.55),
            param_f32(node, "fog_color_b", 0.65),
        ),
        height_falloff_addr: ParamAddr::root(node.id, "height_falloff"),
        height_falloff_value: param_f32(node, "height_falloff", 0.0),
        height_falloff_driven: level.producer(node.id, "height_falloff").is_some(),
        ambient_tint_addr: (
            ParamAddr::root(node.id, "ambient_tint_r"),
            ParamAddr::root(node.id, "ambient_tint_g"),
            ParamAddr::root(node.id, "ambient_tint_b"),
        ),
        ambient_tint_value: (
            param_f32(node, "ambient_tint_r", 1.0),
            param_f32(node, "ambient_tint_g", 1.0),
            param_f32(node, "ambient_tint_b", 1.0),
        ),
    }))
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
                assert_eq!(row.hdri_file_addr.node_doc_id, 4);
            }
            other => panic!("expected Importer shape, got {other:?}"),
        }
        match vm.camera {
            CameraVm::Orbit(row) => {
                assert_eq!(row.node_doc_id, 1);
                assert_eq!(row.lens.as_ref().map(|l| l.node_doc_id), Some(2));
                assert_eq!(row.orbit_value, 0.7, "picks up the orbit atom's own default");
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
    fn hand_built_object_group_degrades_to_custom_row_when_unparseable() {
        // A render_scene wired directly to a bare mesh source (no group) —
        // the D3 "Object k — custom (edit in graph)" case.
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![mesh, scene, out],
            vec![wire(1, "vertices", 10, "mesh_0"), wire(10, "color", 20, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.objects.len(), 1);
        assert!(matches!(vm.objects[0], SceneObjectVm::Custom { index: 0, .. }));
    }

    #[test]
    fn custom_object_with_root_level_transform_resolves_it() {
        // The D3 Custom-row transform fallback: no group at all, but
        // `transform_0` traces to a bare `node.transform_3d` sibling at
        // root — still shows a transform row, per D3's "transform row if
        // transform_k traces to a transform_3d".
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let mesh = node(1, "node.cube_mesh", Some("mesh"));
        let transform = with_param(node(2, TRANSFORM_3D_TYPE_ID, Some("t")), "pos_x", SerializedParamValue::Float { value: 4.0 });
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![mesh, transform, scene, out],
            vec![
                wire(1, "vertices", 10, "mesh_0"),
                wire(2, "transform", 10, "transform_0"),
                wire(10, "color", 20, "in"),
            ],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match &vm.objects[0] {
            SceneObjectVm::Custom { transform, .. } => {
                let t = transform.as_ref().expect("root-level transform_3d resolves for a Custom row");
                assert_eq!(t.node_doc_id, 2);
                assert_eq!(t.pos_value.0, 4.0);
                assert!(t.pos_addr.0.scope_path.is_empty(), "root-level transform has an empty scope");
            }
            other => panic!("expected Custom object, got {other:?}"),
        }
    }

    #[test]
    fn named_group_object_resolves_with_modifier_chain_material_and_transform() {
        // Matches the SHIPPED shape (`AddSceneObjectCommand`, the glTF
        // importer): mesh/material/transform all live INSIDE the group; the
        // group's own `transform`/`material` INTERFACE OUTPUTS pass through
        // to the root wire render_scene reads — the root-level producer of
        // `transform_0` is the GROUP, never a bare `node.transform_3d`
        // sibling (that shape is the D3 Custom fallback only).
        let group_iface = GroupInterface { inputs: vec![], outputs: vec![], params: vec![] };
        let mesh = node(101, "node.cube_mesh", Some("mesh"));
        let bend = node(102, "node.bend_mesh", Some("bend"));
        let mat = with_param(node(103, "node.phong_material", Some("mat")), "color_r", SerializedParamValue::Float { value: 0.4 });
        let transform = with_param(node(105, TRANSFORM_3D_TYPE_ID, Some("transform")), "pos_y", SerializedParamValue::Float { value: 2.5 });
        let gout = node(104, manifold_core::effect_graph_def::GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(1, manifold_core::effect_graph_def::GROUP_TYPE_ID, Some("Hero"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, bend, mat, transform, gout],
            wires: vec![
                wire(101, "vertices", 102, "in"),
                wire(102, "out", 104, "vertices"),
                wire(103, "out", 104, "material"),
                wire(105, "transform", 104, "transform"),
            ],
            tint: Some([0.1, 0.2, 0.3, 1.0]),
        }));

        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(20, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![
                wire(1, "vertices", 10, "mesh_0"),
                wire(1, "transform", 10, "transform_0"),
                wire(10, "color", 20, "in"),
            ],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.objects.len(), 1);
        match &vm.objects[0] {
            SceneObjectVm::Known { group_node_id, name, tint, modifier_chain, material, transform, .. } => {
                assert_eq!(*group_node_id, 1);
                assert_eq!(name, "Hero");
                assert_eq!(*tint, Some([0.1, 0.2, 0.3, 1.0]));
                assert_eq!(modifier_chain.len(), 1);
                assert_eq!(modifier_chain[0].type_id, "node.bend_mesh");
                match material {
                    MaterialVm::Known(row) => {
                        assert_eq!(row.type_id, "node.phong_material");
                        assert_eq!(row.base_color_addr.0.scope_path, vec![1], "material lives inside the group — scoped address");
                    }
                    MaterialVm::None => panic!("expected a resolved material"),
                }
                let t = transform.as_ref().expect("transform_0 traces through the group to node 105");
                assert_eq!(t.node_doc_id, 105);
                assert_eq!(t.pos_value.1, 2.5);
                assert_eq!(t.pos_addr.1.scope_path, vec![1], "transform lives inside the group — scoped address");
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
        let gout = node(3, manifold_core::effect_graph_def::GROUP_OUTPUT_TYPE_ID, Some("output"));
        let mut group_node = node(10, manifold_core::effect_graph_def::GROUP_TYPE_ID, Some("Pbr"));
        group_node.group = Some(Box::new(GroupDef {
            interface: group_iface,
            nodes: vec![mesh, mat, gout],
            wires: vec![wire(1, "vertices", 3, "vertices"), wire(2, "out", 3, "material")],
            tint: None,
        }));
        let scene = with_param(node(20, RENDER_SCENE_TYPE_ID, None), "objects", SerializedParamValue::Float { value: 1.0 });
        let out = node(30, "system.final_output", None);
        let d = def(
            vec![group_node, scene, out],
            vec![wire(10, "vertices", 20, "mesh_0"), wire(20, "color", 30, "in")],
        );
        let vm = SceneVm::from_def(&d).unwrap();
        match &vm.objects[0] {
            SceneObjectVm::Known { material: MaterialVm::Known(row), .. } => {
                let mr = row.metallic_roughness.as_ref().expect("pbr gets metallic/roughness");
                assert_eq!(mr.metallic_value, 0.7);
                assert_eq!(mr.roughness_value, 0.3);
            }
            other => panic!("expected Known pbr object, got {other:?}"),
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
    fn known_light_row_resolves_full_param_addresses_and_values() {
        let light = with_param(
            with_param(
                with_param(node(8, LIGHT_TYPE_ID, Some("sun")), "intensity", SerializedParamValue::Float { value: 2.5 }),
                "cast_shadows",
                SerializedParamValue::Float { value: 1.0 },
            ),
            "shadow_softness",
            SerializedParamValue::Enum { value: 3 }, // Contact
        );
        let light = with_param(light, "light_size", SerializedParamValue::Float { value: 4.0 });
        let scene = with_param(node(10, RENDER_SCENE_TYPE_ID, None), "lights", SerializedParamValue::Float { value: 1.0 });
        let out = node(20, "system.final_output", None);
        let d = def(vec![light, scene, out], vec![wire(8, "out", 10, "light_0"), wire(10, "color", 20, "in")]);
        let vm = SceneVm::from_def(&d).unwrap();
        assert_eq!(vm.lights.len(), 1);
        match &vm.lights[0] {
            SceneLightVm::Known(row) => {
                assert_eq!(row.node_doc_id, 8);
                assert_eq!(row.intensity_value, 2.5);
                assert!(row.cast_shadows_value, "cast_shadows > 0.5 reads as on");
                assert_eq!(row.shadow_softness_value, 3, "Contact");
                assert_eq!(row.light_size_value, 4.0, "light_size resolves even though it's a Contact-only knob — parameter dependency, not conditional UI");
                assert!(!row.intensity_driven);
                assert_eq!(row.intensity_addr, ParamAddr::root(8, "intensity"));
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
    fn free_camera_with_lens_pass_through_traces_full_rows() {
        let cam = with_param(node(1, FREE_CAMERA_TYPE_ID, Some("camera")), "yaw", SerializedParamValue::Float { value: 1.2 });
        let lens = with_param(node(2, CAMERA_LENS_TYPE_ID, Some("lens")), "f_stop", SerializedParamValue::Float { value: 2.8 });
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
                assert_eq!(row.yaw_value, 1.2);
                let lens_row = row.lens.as_ref().expect("lens pass-through resolves");
                assert_eq!(lens_row.node_doc_id, 2);
                assert_eq!(lens_row.f_stop_value, 2.8);
            }
            other => panic!("expected Free camera, got {other:?}"),
        }
    }

    #[test]
    fn look_at_camera_shape_traces_pos_target_fov() {
        let cam = with_param(
            node(1, LOOK_AT_CAMERA_TYPE_ID, Some("camera")),
            "target_y",
            SerializedParamValue::Float { value: 1.5 },
        );
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
                assert_eq!(row.target_value.1, 1.5);
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
}
