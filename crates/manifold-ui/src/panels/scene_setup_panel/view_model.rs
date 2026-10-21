//! The data the Scene tab is built from: row addresses and values, the per-item
//! view models, and the selection type. Pure data; the panel draws from it.

use super::*;

/// A single editable node-param address: the exact `(scope_path,
/// node_doc_id, param_id)` triple `SetGraphNodeParamCommand::with_scope`
/// takes. `scope_path` is empty for every P1 row (Environment/Fog, and
/// Objects' root-level transform_3d rows) and `[group_node_id]` for a P2
/// Objects material/modifier row living inside the object's own group.
#[derive(Clone, Debug, PartialEq)]
pub struct RowAddr {
    pub scope_path: Vec<u32>,
    pub node_doc_id: u32,
    pub param_id: String,
}

impl RowAddr {
    pub fn root(node_doc_id: u32, param_id: &str) -> Self {
        Self { scope_path: Vec::new(), node_doc_id, param_id: param_id.to_string() }
    }
}

/// One numeric row: its write address, current value, range, and whether a
/// wire currently drives it (driven rows render read-only — D4).
#[derive(Clone, Debug, PartialEq)]
pub struct RowValue {
    pub addr: RowAddr,
    pub value: f32,
    pub min: f32,
    pub max: f32,
    pub driven: bool,
    /// Whether this param is an exposed card param on the layer's generator
    /// graph (`scene_vm::is_param_exposed`). Read-only here: exposure is a
    /// graph-editor toggle.
    pub exposed: bool,
}

/// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a (D3): the driver/envelope/
/// audio-mod facts for one Environment/Fog row, flattened by the app layer's
/// `row_modulation_for_id` from `lookup_param_mod_for_id`'s
/// `(RowMod, AudioRowState)` scalar result — this crate has no
/// `PresetInstance`, so the app computes this and hands it across the VM
/// boundary like every other field here. Field-for-field the same facts
/// `ParamModState`/`AudioRowState` carry per-row; a plain idle default
/// (`Default::default()`) means "no modulation," never an error.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RowModulation {
    pub driver_active: bool,
    pub trim_min: f32,
    pub trim_max: f32,
    pub driver_beat_div_idx: i32,
    pub driver_waveform_idx: i32,
    pub driver_reversed: bool,
    pub driver_dotted: bool,
    pub driver_triplet: bool,
    pub driver_free_period: Option<f32>,
    pub driver_frame_aligned: bool,
    /// Effective cycle frames and Hz, projected only when frame alignment is on.
    pub driver_frame_rate: Option<(u32, f32)>,
    pub envelope_active: bool,
    pub target_norm: f32,
    pub env_decay: f32,
    pub envelope_action_idx: i32,
    pub envelope_step_amount: f32,
    pub envelope_wrap_idx: i32,
    pub automation_active: bool,
    pub automation_overridden: bool,
    pub audio_active: bool,
    pub audio_send_id: Option<AudioSendId>,
    pub audio_kind_idx: i32,
    pub audio_band_idx: i32,
    pub audio_range_min: f32,
    pub audio_range_max: f32,
    pub audio_invert: bool,
    pub audio_rate: bool,
    pub audio_sensitivity: f32,
    pub audio_attack_ms: f32,
    pub audio_release_ms: f32,
    pub audio_trigger_mode_idx: i32,
    pub audio_action_idx: i32,
    pub audio_step_amount: f32,
    pub audio_wrap_idx: i32,
}

/// A [`RowValue`] paired with its [`RowModulation`] — the shape every
/// `build_param_row`-converted row needs (C-P1a: Environment/Fog only; other
/// families still carry a bare `RowValue` until their own sub-phase
/// converts them).
#[derive(Clone, Debug, PartialEq)]
pub struct ModulatedRow {
    pub value: RowValue,
    /// Boxed — `RowModulation` is ~30 scalar fields; unboxed it would make
    /// `EnvironmentRowVm`/`AtmosphereRowVm` (which carry 2-4 `ModulatedRow`s
    /// per variant, alongside a data-less `None`/`Custom`) a clippy
    /// `large_enum_variant` violation.
    pub modulation: Box<RowModulation>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EnvironmentRowVm {
    /// Importer shape (an HDRI source wired into the bake). Mode is an
    /// ordinary Lighting row; `hdri_param` is the generator's string param
    /// that loads the HDRI file, when the preset declares one.
    Importer { intensity: ModulatedRow, fill: ModulatedRow, hdri_file: String, hdri_param: Option<usize> },
    Bare { intensity: ModulatedRow, fill: ModulatedRow },
    /// Some other producer wired into `envmap` — honest custom row, no
    /// controls (D3).
    Custom,
    /// Unwired — the "Add environment" empty row.
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AtmosphereRowVm {
    Wired { density: ModulatedRow, height_falloff: ModulatedRow },
    /// Unwired — the "Add fog" empty row.
    None,
}

/// One `node.transform_3d`'s "3 compact triplets" (D4): Position/Rotation/
/// Scale, each X/Y/Z a [`ModulatedRow`] — C-P1b (SCENE_PANEL_CARD_CONVERGENCE_
/// DESIGN.md): promoted from a bare `RowValue` so the Object family's
/// converted rows can carry driver/envelope/audio-mod facts through the same
/// `ModulatedRow` shape the Environment/Fog family already uses.
#[derive(Clone, Debug, PartialEq)]
pub struct TransformRowVm {
    pub pos: (ModulatedRow, ModulatedRow, ModulatedRow),
    pub rot: (ModulatedRow, ModulatedRow, ModulatedRow),
    pub scale: (ModulatedRow, ModulatedRow, ModulatedRow),
}

/// The Objects section's material quick-knob row (D3/D4): base color always,
/// metallic/roughness only for `pbr_material` (unlit/cel don't have
/// that param — "the atom's own params otherwise"). C-P1b: `ModulatedRow`,
/// same promotion as [`TransformRowVm`].
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectMaterialVm {
    Pbr { color: (ModulatedRow, ModulatedRow, ModulatedRow), metallic: ModulatedRow, roughness: ModulatedRow },
    Other { color: (ModulatedRow, ModulatedRow, ModulatedRow) },
    /// No material resolved on this object.
    None,
}

/// App-adapted texture ownership facts for the selected object's material.
/// The UI intentionally receives labels and connection state only; readiness
/// and asset probing stay out of this DTO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialTextureInfo {
    pub port: String,
    pub label: String,
    pub source_label: String,
    pub connected: bool,
    pub graph_source: bool,
}

/// Structural material facts shown beside the manifest-backed material rows.
/// Assignment remains object-owned while placement rows remain material-owned.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaterialInspectorInfo {
    pub object: SceneNodeRef,
    pub object_gain: Option<manifold_foundation::ParamId>,
    pub material: SceneNodeRef,
    pub shared_object_count: Option<usize>,
    /// The starter look the material currently matches; `None` is Custom.
    pub look: Option<MaterialLook>,
    pub textures: Vec<MaterialTextureInfo>,
    /// Exact inner material parameter bindings for the selected material.
    /// Names are descriptor keys; ids are the exposed graph parameters.
    pub params: Vec<(String, manifold_foundation::ParamId)>,
}

/// One modifier-stack entry (D6/P5): the atom's display name, its own
/// address, and its curated param rows. `index` is this modifier's 0-based
/// position in wire order (source → … → output) — the same convention
/// `InsertMeshModifierCommand::position`/`MoveMeshModifierCommand::new_position`
/// take, and what the up/down buttons compute against.
#[derive(Clone, Debug, PartialEq)]
pub struct ModifierKnownRow {
    pub index: usize,
    pub node_doc_id: u32,
    pub display_name: String,
    /// Exact exposed parameter ids owned by this modifier. These ids come
    /// from projection metadata and may be custom/non-numeric; UI must never
    /// infer ownership from a numeric document-id prefix.
    pub parameter_ids: Vec<String>,
}

// P4b: the Skin row's payload types live in `scene_setup_skin.rs` (this file
// is under the godfile line ceiling); re-exported here so panel-qualified
// paths stay put.
pub use super::super::scene_setup_skin::{SkinRowVm, SkinTargetMap};

/// Payload for [`ObjectRowVm::Known`], boxed so the enum's footprint tracks
/// the small `Custom` variant instead of this one (clippy
/// `large_enum_variant` — same convention as `LightRow`/`OrbitCameraRow` in
/// `scene_vm.rs`).
#[derive(Clone, Debug, PartialEq)]
pub struct ObjectKnownRow {
    /// Where the row sits (the scene object model's kind): a Water look owns
    /// only its look.
    pub kind: manifold_foundation::scene_surface::SceneObjectRowKind,
    /// What the row's header, shortcuts and commands accept.
    pub capabilities: manifold_foundation::scene_surface::SceneObjectCapabilities,
    pub index: usize,
    /// The row's stable address: the scene object (the group, for a virtual
    /// parent). Selection, keys, rename, remove, duplicate and physics all
    /// name the row by it; document ids repeat across import groups.
    pub node: SceneNodeRef,
    /// The `node.scene_object`'s own doc id, for the id-addressed modifier
    /// and skin writes. Never an identity.
    pub object_node_id: u32,
    /// The node the row's modifier chain hangs off: its group, else the
    /// object itself.
    pub modifier_owner: SceneNodeRef,
    /// A virtual compound parent row. Parent rows precede their physical
    /// children in the renderer's flat preorder list.
    pub is_group: bool,
    /// The compound parent for a child row. `None` means a top-level object
    /// (including a virtual group parent).
    pub parent: Option<SceneNodeRef>,
    pub name: String,
    pub visible: RowValue,
    pub transform: Option<Box<TransformRowVm>>,
    pub material: ObjectMaterialVm,
    /// Material inspector facts for this selected object, when the renderer
    /// resolved a known material producer.
    pub material_inspector: Option<MaterialInspectorInfo>,
    /// The modifier stack, in wire order (D6/P5) — the interactive list the
    /// panel renders with add/remove/reorder. Not a stored value: rebuilt
    /// from the Vm's own `modifier_chain` trace every sync (D1).
    pub modifiers: Vec<ModifierKnownRow>,
    /// `false` when the trace couldn't parse this object's mesh chain at all
    /// (D6: "custom chain — edit in graph") — the panel shows that label and
    /// disables "Add modifier" for THIS object only, never a blind splice
    /// into unrecognized topology. `true` (even with an empty `modifiers`
    /// list) means the stack is well-formed and addable.
    pub modifiers_addable: bool,
    /// P4b: the object's layer-skin row. `None` when no `node.layer_source`
    /// is wired into the object's material maps.
    pub skin: Option<SkinRowVm>,
    pub physics_enabled: bool,
    pub physics_available: bool,
    pub physics_unavailable_reason: Option<String>,
    /// The row is a copy group: it draws its copies body's instances, and
    /// its card carries the Copies rows instead of a Make Copies action.
    pub copy_group: bool,
    /// Make Copies can turn this row into a copy group.
    pub copies_available: bool,
    /// Why it cannot, when the row would otherwise offer it.
    pub copies_unavailable_reason: Option<String>,
    pub physics_imported: bool,
    pub fluid_role_available: bool,
    pub fluid_roles: Result<Vec<FluidRoleRow>, String>,
    /// The liquid's lattice as its Resolution and box resolve it; `None`
    /// for anything that is not water, or water whose box is wire-driven.
    pub lattice: Option<LatticeReadout>,
}

/// What a liquid's Resolution buys: the cell grid and the cell's edge.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LatticeReadout {
    pub cells: [u32; 3],
    pub cell_size_m: f32,
}

impl LatticeReadout {
    /// One line for the panel: the grid, the cell edge in centimetres and
    /// the cell count.
    pub fn line(&self) -> String {
        let [x, y, z] = self.cells;
        let count = u64::from(x) * u64::from(y) * u64::from(z);
        format!("Lattice {x} × {y} × {z} · {:.1} cm cells · {count} cells", self.cell_size_m * 100.0)
    }
}

/// One Objects-section row (D3/D4).
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectRowVm {
    /// Producer resolved to a `node.scene_object` (D12), directly or through
    /// one wrapping group.
    Known(Box<ObjectKnownRow>),
    /// Producer did NOT resolve to a `node.scene_object` — "Object k —
    /// custom (edit in graph)" per D3/D12.
    Custom { index: usize },
}

/// One light row: the fields the panel reads.
#[derive(Clone, Debug, PartialEq)]
pub struct LightKnownRow {
    pub index: usize,
    /// The light's stable address.
    pub node: SceneNodeRef,
    pub node_doc_id: u32,
    /// P5: the light's editable display name (NEW — lights didn't have one
    /// before this design). Double-click opens the same rename UX as an
    /// object's name.
    pub name: String,
}

/// One Lights-section row.
#[derive(Clone, Debug, PartialEq)]
pub enum LightRowVm {
    Known(Box<LightKnownRow>),
    /// Producer wasn't `node.light` — honest custom row (D3).
    Custom { index: usize },
}

/// The Camera section (D3/D4): exactly one of these per live scene (`None`
/// when `camera` is unwired — D3 has no "add camera" action in v1, unlike
/// Environment/Fog, since `render_scene`'s `camera` port is REQUIRED —
/// SCENE_BUILD's starter preset and every importer path always wire one).
#[derive(Clone, Debug, PartialEq)]
pub enum CameraRowVm {
    None,
    Orbit,
    Free,
    LookAt,
    /// Producer resolved but isn't one of the three curated atoms — honest
    /// custom row (D3).
    Custom,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FluidDomainOption {
    /// The domain's stable id: document ids repeat across group levels.
    pub node: FoundationNodeId,
    pub name: String,
    /// The tree rows drawing this water and their parent rows: selecting
    /// any of them makes it the Add menu's target for a fluid source.
    pub rows: Vec<SceneNodeRef>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FluidRoleRow {
    /// The role's stable address.
    pub source: SceneNodeRef,
    pub name: String,
    pub target_label: String,
}

/// Full live-panel view model for one selected generator layer's scene —
/// translated 1:1 from `manifold_nodes::node_graph::scene_vm::SceneVm`'s
/// Header/Environment/Atmosphere sections by `state_sync` (this crate can't
/// depend on `manifold-nodes`/`manifold-core`, so the translation is the
/// UI-facing DTO boundary, same convention as `AudioSendRow`).
#[derive(Clone, Debug, PartialEq)]
pub struct SceneForceRowVm {
    pub instance_id: FoundationNodeId,
    pub title: String,
}

/// One item of the Scene tab tree (SCENE_SURFACES section 3.5 (scene
/// items)), in the order `scene_items` lists them.
#[derive(Clone, Debug, PartialEq)]
pub struct SceneItemVm {
    pub kind: manifold_foundation::scene_surface::SceneItemKind,
    pub selection: SceneSelection,
    pub name: String,
    /// The compound parent object a child object row sits under.
    pub parent: Option<SceneNodeRef>,
    /// The controls this item owns, in manifest order: every exposed control
    /// whose primary binding targets one of the item's nodes.
    pub parameter_ids: Vec<String>,
}

/// What one properties pass shows.
pub(crate) struct PropertyRows<'a> {
    /// The item's controls.
    pub owned: &'a [String],
    /// Controls another host draws (modifier cards).
    pub excluded: &'a [String],
    /// The group of a control the scene table does not describe.
    pub fallback: ControlGroup,
    /// The selected object, for its material block and physics facts.
    pub object: Option<&'a ObjectKnownRow>,
}

impl<'a> PropertyRows<'a> {
    pub(crate) fn new(owned: &'a [String], fallback: ControlGroup) -> Self {
        Self { owned, excluded: &[], fallback, object: None }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct SceneSetupVm {
    pub layer_id: LayerId,
    pub multiple_scenes: bool,
    /// Next object and light slot indices for the add actions.
    pub object_count: usize,
    pub light_count: usize,
    /// `render_scene`'s own doc id — the target the "Add environment"/
    /// "Add fog"/"+ Object"/"+ Light" composites wire into.
    pub scene_root_node_id: u32,
    pub environment: EnvironmentRowVm,
    pub atmosphere: AtmosphereRowVm,
    /// P2: the Objects section's rows, in `mesh_k` order.
    pub objects: Vec<ObjectRowVm>,
    pub fluid_domains: Vec<FluidDomainOption>,
    /// P3: the Lights section's rows, in `light_k` order. Never capped —
    /// REALTIME_3D D4's shadow-caster limit (K=4) is the renderer's job; the
    /// panel reports the true count and renders every row regardless.
    pub lights: Vec<LightRowVm>,
    /// P3: the Camera section (D3's single-camera trace, lens pass-through
    /// included).
    pub camera: CameraRowVm,
    /// Whether this camera's post-processing effects need the one-time scene
    /// setup action before their controls can be used.
    pub camera_setup_needed: bool,
    /// Every item of the scene, in tree order: Camera, lights, objects,
    /// forces, physics worlds, World.
    pub items: Vec<SceneItemVm>,
    /// Scene bounds for translate-slider range derivation. `Some((min, max))`
    /// when bounds are available (stored import bounds or camera-distance proxy),
    /// used to compute scene-relative slider ranges (center ± 2×extent per axis).
    /// Fallback to descriptor defaults when None.
    pub scene_bounds: Option<([f32; 3], [f32; 3])>,
    /// Scene force rows projected with stable instance identities.
    pub forces: Vec<SceneForceRowVm>,
    /// Typed force recipe picker for the selected scene layer.
    pub force_picker: Vec<ModifierPickerEntry>,
}

/// P5's outliner selection (D7): the one scene item whose controls the
/// properties region shows. UI-local workspace state — like fold state,
/// NEVER serialized (`rg -n "SceneSelection" crates/manifold-io
/// crates/manifold-core` must stay 0 hits). Object and light payloads
/// are `SceneNodeRef`s and force and physics payloads are `NodeId`s: stable
/// graph identities, unlike indices or document ids.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SceneSelection {
    Object(SceneNodeRef),
    /// One light, by its address. It names the light's item; the tree and the
    /// properties show every light under [`SceneSelection::Lighting`].
    Light(SceneNodeRef),
    Force(FoundationNodeId),
    Camera,
    /// All lighting in one place: the environment and its HDRI, exposure,
    /// and every light.
    Lighting,
    World,
    /// A physics world, by its node.
    Physics(FoundationNodeId),
    /// A placed fluid source (Inflow, Drain or Collider), by its role node.
    Source(FoundationNodeId),
    /// A tree group's fold toggle; selecting it changes no selection.
    OutlinerFold(SceneGroup),
}

/// D7's four empty/live states for the selected layer.
#[derive(Clone, Debug, PartialEq)]
pub enum SceneSetupState {
    /// Nothing selected, or the selection isn't a generator layer — one
    /// sentence naming what to select.
    NoSelection(String),
    /// A generator layer with no generator assigned (or an empty slot).
    NoGenerator { layer_id: LayerId },
    /// A generator layer whose graph has no `render_scene`.
    NoScene { layer_id: LayerId },
    /// The full panel.
    Live(Box<SceneSetupVm>),
}

impl Default for SceneSetupState {
    fn default() -> Self {
        SceneSetupState::NoSelection("Select a layer to set up its scene.".to_string())
    }
}

impl SceneSetupState {
    /// Extract the `Live` VM if present, None otherwise.
    pub fn as_live(&self) -> Option<&SceneSetupVm> {
        match self {
            SceneSetupState::Live(vm) => Some(vm),
            _ => None,
        }
    }
}
