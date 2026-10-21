//! The Scene tab of the inspector (SCENE_SURFACES_DESIGN.md D8 (Scene tab
//! tree), D16 (the scene is a tab in the inspector)).
//!
//! The tree of scene items sits in its own pane on top (`outliner.rs`), and
//! the selected item's header (`item_header.rs`) and controls, grouped by
//! control group, scroll in a second pane below a draggable split
//! (`panes.rs`). The panel builds `UITree` nodes from data handed in via
//! [`ScenePanel::configure`] and maps clicks and drags to
//! [`super::PanelAction`]; it never writes the project itself. Param rows
//! are the shared card rows, dispatched through the same `RowHost` every
//! card uses.

mod camera;
#[path = "scene_setup_panel/outliner.rs"]
mod outliner;
pub use outliner::SceneGroup;
#[path = "scene_setup_panel/item_header.rs"]
mod item_header;
#[path = "scene_setup_panel/copies.rs"]
mod copies;
#[path = "scene_setup_panel/keyboard.rs"]
mod keyboard;
pub use keyboard::SceneItemAddress;
#[cfg(test)]
mod trim_tests;
#[cfg(test)]
mod host_parity_tests;
#[cfg(test)]
mod row_gesture_tests;
#[path = "scene_setup_panel/material_inspector.rs"]
mod material_inspector;
mod material_menus;
#[path = "scene_setup_panel/object_modifiers.rs"]
mod object_modifiers;
#[path = "scene_setup_panel/forces.rs"]
mod forces;
#[path = "scene_setup_panel/parameter_navigation.rs"]
mod parameter_navigation;
#[path = "scene_setup_panel/value_sync.rs"]
mod value_sync;
#[path = "scene_setup_panel/lighting.rs"]
mod lighting;
#[path = "scene_setup_panel/fluid_roles.rs"]
mod fluid_roles;
mod panes;

use crate::{ProjectAction, RootAction};
use crate::chrome::{ChromeHost, View};
use crate::color;
use crate::input::UIEvent;
use crate::node::*;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};
use crate::tree::UITree;
use manifold_foundation::{AudioSendId, LayerId, NodeId as FoundationNodeId};

use super::{GraphParamTarget, PanelAction, ParamsAction};
use super::actions::{MaterialEditKind, MaterialParamWrite};
#[cfg(test)]
use super::{ScrubPhase, ScrubValue, ValueRef};
use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_card::{ParamCardPanel, RowGeometry, RowMod};
use super::param_slider_shared::{
    AudioRowState, ModTab, ParamModState, RowHost, RowInteraction, build_param_row,
    ROW_ROLE_TOGGLE, build_colour_row, param_row_key_base,
    build_button_row, toggle_btn_style,
};
use crate::param_surface::{
    MaterialGroup, MaterialLook, MaterialMapFamily, MaterialParamRole, ObjectModifierCardInfo,
    ModifierPickerEntry, ParamRow, ParamSurface, RowKind, RowMapping,
    RowRole, RowSpec,
};
use manifold_foundation::SceneNodeRef;
use crate::slider::GAP;
use manifold_foundation::scene_surface::{ControlGroup, Feature, Gate, SurfaceRole};

// ── Stable keys ──
const KEY_BG: u64 = 80_001;
const KEY_ADD_ENVIRONMENT: u64 = 80_010;
const KEY_ADD_FOG: u64 = 80_011;
const KEY_NEW_SCENE: u64 = 80_012;
const KEY_OPEN_GRAPH_EDITOR: u64 = 80_013;
const KEY_CAMERA_SETUP: u64 = 80_025;
const KEY_SHOW_GIZMOS: u64 = 80_026;
/// Frame button offset: use offset 33 to avoid collision with Remove (20), Duplicate (21), and mod buttons (22..32)
const OBJ_OFF_FRAME: u64 = 33;
/// P4b Skin row source/target dropdown buttons.
const OBJ_OFF_SKIN_SOURCE: u64 = 34;
const OBJ_OFF_SKIN_TARGET: u64 = 35;
const OBJ_OFF_PHYSICS: u64 = 36;
const OBJ_OFF_FLUID_ROLE: u64 = 37;
/// Water-family child Hide button; unlike the outliner eye this is a
/// one-way local write and is always present, including for hidden rows.
const OBJ_OFF_HIDE: u64 = 40;
const MATERIAL_LOOK_KEY_BASE: u64 = 97_000;

/// Per-object dynamic keys: `OBJ_KEY_BASE + index * OBJ_KEY_STRIDE + offset`.
/// Objects are a variable-length list (unlike the four fixed Environment/Fog
/// rows above), so every object gets a
/// private key range wide enough for its expand toggle, name, and its
/// numeric controls (3 triplets + color + metallic + roughness) plus, as of
/// UX-P3a, one mod-button key per exposable field.
const OBJ_KEY_BASE: u64 = 82_000;
// UX-P3a: bumped 32→44 to fit 11 new mod-button offsets (22..32) alongside
// the existing 0..21 range — `OBJ_KEY_BASE`'s 2_000-wide gap to
// `LIGHT_KEY_BASE` still covers 45 objects at this stride, well past any
// real scene (`typical-project-scale`: dozens of layers, not objects).
const OBJ_KEY_STRIDE: u64 = 44;
const OBJ_OFF_NAME: u64 = 1;
/// BUG-193 per-row "✕" remove button, on the title row next to the name.
const OBJ_OFF_REMOVE: u64 = 20;

const fn obj_key(index: usize, offset: u64) -> u64 {
    OBJ_KEY_BASE + index as u64 * OBJ_KEY_STRIDE + offset
}

/// A scene address as plain text, for widget keys and widget names.
pub(crate) fn ref_text(node: &SceneNodeRef) -> String {
    let mut text = String::new();
    for group in &node.scope {
        text.push_str(group.as_str());
        text.push('/');
    }
    text.push_str(node.node.as_str());
    text
}
/// Per-light dynamic keys (P3), same convention as `obj_key`: Lights is a
/// variable-length list, so every light gets a private key range.
const LIGHT_KEY_BASE: u64 = 84_000;
// UX-P3b-i: bumped 32→44 — same fix `OBJ_KEY_STRIDE` needed for P3a, applied
// here by the collision audit this phase's brief calls out. Bumping also
// retired a pre-existing bug the audit found while sizing the new range: the
// light-name button (`build_light_properties_header`) used to key itself at
// `light_key(index, LIGHT_OFF_MODE_MINUS) + 100`, an out-of-stride offset
// that reached 100 slots past its own light's 0..31 range and, at the OLD
// stride of 32, landed exactly on light (index+3)'s Color-G cell
// (`light_key(index, 1) + 100 == light_key(index + 3, LIGHT_OFF_COLOR_R + 1)`
// — both equal `LIGHT_KEY_BASE + index*32 + 101`) whenever a scene had 4+
// lights. `LIGHT_OFF_NAME` below replaces the hack with a real in-range
// offset; no scene with 4+ lights ever exercised the old collision in a
// shipped flow, but it was live in the read path.
const LIGHT_KEY_STRIDE: u64 = 44;
/// BUG-193 per-row "✕" remove button, on the title row next to the label.
const LIGHT_OFF_REMOVE: u64 = 26;
/// UX-P3b-i: the light-name drag/rename button's own offset, replacing the
/// `LIGHT_OFF_MODE_MINUS + 100` out-of-stride hack (see the stride comment
/// above).
const LIGHT_OFF_NAME: u64 = 27;

const fn light_key(index: usize, offset: u64) -> u64 {
    LIGHT_KEY_BASE + index as u64 * LIGHT_KEY_STRIDE + offset
}

/// D6's curated "Add modifier" vocabulary: `(display name, type_id)`, in the
/// design's own order. Plain string literals — no `manifold-nodes`
/// dependency needed here; the command that receives the chosen `type_id`
/// (`InsertMeshModifierCommand`, `manifold-editing`) is what actually knows
/// it names a real primitive.
pub const MESH_MODIFIER_CHOICES: &[(&str, &str)] = &[
    ("Bend", "node.bend_mesh"),
    ("Twist", "node.twist_mesh"),
    ("Taper", "node.taper_mesh"),
    ("Inflate", "node.push_along_normals"),
    ("Displace by Texture", "node.push_mesh"),
    ("Morph", "node.morph_mesh"),
    ("Rotate", "node.rotate_3d"),
];

/// Modifier-stack dynamic keys (P5) — nested two levels (object index ×
/// modifier slot within that object), unlike `obj_key`'s single-level
/// stride: each object gets a generous per-object budget wide enough for
/// several modifier rows (remove/up/down + up to 4 param cells each) PLUS
/// the single "+ Add Modifier" button (UX-P2 D6 — was a 7-chip grid),
/// reserved in its own sub-range so neither can collide with the other as
/// the stack grows.
const MODIFIER_KEY_BASE: u64 = 88_000;
const MODIFIER_OBJ_STRIDE: u64 = 480;
/// Reserved sub-range within the per-object budget for the "+ Add Modifier"
/// button (UX-P2 D6: one control now, was 7 chips) — well clear of any real
/// modifier stack (never more than a handful of rows).
const MODIFIER_ADD_BUTTON_OFFSET: u64 = 400;

const fn modifier_add_button_key(ordinal: usize) -> u64 {
    MODIFIER_KEY_BASE + ordinal as u64 * MODIFIER_OBJ_STRIDE + MODIFIER_ADD_BUTTON_OFFSET
}

const PANEL_W_MIN: f32 = 320.0;
pub(crate) const ROW_H: f32 = 24.0;
pub(crate) const ROW_GAP: f32 = 4.0;
const PAD: f32 = 10.0;
const STEP_W: f32 = 22.0;

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
    /// Importer shape (switch_texture selecting Softbox/HDRI) — Mode is
    /// shown as a static chip in P1 (toggling it is a P2+ affordance; the
    /// value is legible, just not yet a control here).
    /// `hdri_param` is the generator's string param that loads the HDRI
    /// file, when the preset declares one.
    Importer { mode_is_hdri: bool, intensity: ModulatedRow, fill: ModulatedRow, hdri_file: String, hdri_param: Option<usize> },
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
pub use super::scene_setup_skin::{SkinRowVm, SkinTargetMap};

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
    /// Free body slots in the Physics World this object's body sits in;
    /// `None` without physics. A copy count above it cannot fit.
    pub physics_free_body_slots: Option<u32>,
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

/// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md C-P1a: the panel's card-shaped row
/// state for a converted family — D2's synthesized-id map, D3's per-row
/// modulation bookkeeping (the same node-id vectors `ParamCardPanel` keeps,
/// scoped down to one family), and D4's drag cadence, all built ONCE here so
/// every later family's own sub-phase reuses this same struct instead of
/// re-deriving it. C-P1a populates exactly one instance — the four
/// Environment/Fog rows, at FIXED indices `WORLD_ENV_INTENSITY`..
/// `WORLD_FOG_HEIGHT_FALLOFF` regardless of whether that row is wired this
/// frame (so arming a driver on Fog Density can't silently jump onto
/// Environment Intensity's slot just because Environment got unwired in
/// between — D3's "no scene-local driven cache beyond it" reads off the SAME
/// per-row facts every frame, but the row's IDENTITY — its index — must stay
/// stable across frames independent of which sections are currently wired).
struct SceneCardState {
    rows: Vec<ParamRow>,
    mod_state: ParamModState,
    /// P2 slice 2a: per-row CURRENT value cache for the unified properties
    /// card's real-param rows — seeded from `ParamRow.value.base` at
    /// `configure_from_filtered`, kept fresh every frame by
    /// `ScenePanel::sync_properties_values`. Multi-writer: the drag and
    /// type-in paths also write it mid-gesture, so it is NOT a safe
    /// dirty-check key — `last_pushed_values` is that.
    current_values: Vec<f32>,
    /// Dirty-check key for `sync_properties_values`: the last value this
    /// sync actually PUSHED to the tree. Single-writer (only the sync
    /// touches it), mirroring `ParamCardPanel`'s `param_cache`. The tree is
    /// minted fresh every frame and `build_properties_row` DRAWS this value
    /// at build time, so a skipped push must find it already drawn or the
    /// row snaps back to the default — gating on `current_values` regressed
    /// exactly that way (the post-commit push was skipped and the formatted
    /// text never updated). New slots seed NaN (the sync's `is_nan` clause
    /// forces their first push); `configure_from_filtered` carries values
    /// over by row id so an index that changes identity never shows a stale
    /// row's value.
    last_pushed_values: Vec<f32>,
    /// BUG-313: param id → local row index, rebuilt in `configure_from_filtered`
    /// from the retained rows. The JOIN KEY for the per-frame value sync —
    /// `sync_properties_values` iterates the layer's full generator manifest
    /// (id-keyed) and pushes each slot onto the row this map resolves. A
    /// manifest param the current outliner selection isn't showing simply
    /// misses the map and is skipped; no positional retained-index list, so
    /// nothing can drift (the class BUG-313 removed).
    row_id_index: ahash::AHashMap<String, usize>,
    /// Per-frame reused coverage scratch for the id-join miss invariant
    /// (INV-6): `true` for each row that received a value this sync. A row left
    /// `false` is a built scene row whose id has no live manifest entry.
    row_value_synced: Vec<bool>,
    /// Always `None` — no OSC address surface on scene rows this phase. Kept
    /// panel-side (like `ParamCardPanel::osc_addresses`) and passed by ref to
    /// `RowHost::row_action`, which reads it to gate the label-copy path.
    osc_addresses: Vec<Option<String>>,
    mod_active_tab: Vec<ModTab>,
    /// The shared per-row id-bundle machinery + reverse `WidgetId → (row,
    /// role)` index + click→`PanelAction` routing — the SAME [`RowHost`]
    /// `ParamCardPanel` embeds (P-S3 host unification). Scene rows get their
    /// widget-id bundles, `reindex_row` reverse-indexing, right-click-reset
    /// intent replay and `row_action` routing from here instead of the
    /// hand-copied twin this replaced. The card's own MODEL (rows, mod_state,
    /// values, drag cadence) stays above; only the id bookkeeping and click
    /// logic delegate — the same own-model/shared-machinery split
    /// `ParamCardPanel` keeps.
    row_host: RowHost,
}

impl SceneCardState {
    fn new() -> Self {
        Self {
            rows: Vec::new(),
            mod_state: ParamModState::allocate(0),
            current_values: Vec::new(),
            last_pushed_values: Vec::new(),
            row_id_index: ahash::AHashMap::new(),
            row_value_synced: Vec::new(),
            osc_addresses: Vec::new(),
            mod_active_tab: Vec::new(),
            row_host: RowHost::new(),
        }
    }

    /// Resize every per-row vector to `n`. `mod_state` is rebuilt (the build
    /// pass re-syncs its model facts every frame; its session-only flags
    /// survive inside `ParamModState::rebuild`).
    /// `mod_active_tab` survives a mid-gesture rebuild; the gesture itself is
    /// owned by `RowHost`, which captures the wire address rather than a row
    /// index and therefore remains valid across structural snapshots.
    fn resize(&mut self, n: usize) {
        self.rows.resize(n, placeholder_param_info());
        self.mod_state.rebuild(n);
        self.current_values.resize(n, 0.0);
        // NaN seed = "never pushed"; the sync's is_nan clause forces the first
        // push for a fresh slot. Existing slots keep their value across the
        // per-frame rebuilds (resize_with only fills new entries).
        self.last_pushed_values.resize_with(n, || f32::NAN);
        self.row_value_synced.resize(n, false);
        self.osc_addresses.resize(n, None);
        while self.mod_active_tab.len() < n {
            self.mod_active_tab.push(ModTab::Driver);
        }
        self.row_host.resize(n);
        self.row_host.row_index.clear();
        // The id join must never point past the truncated per-row vectors.
        self.row_id_index.retain(|_, row| *row < n);
    }

    /// Forget the node ids from the last build. Value caches and the id join
    /// (`last_pushed_values`, `row_id_index`) stay, so the next build of the
    /// same rows carries its values over.
    fn clear_nodes(&mut self) {
        self.row_host.clear_nodes();
    }

    /// P2 slice 2a: populate this card from a FILTERED slice of the layer's
    /// real generator [`ParamSurface`] — `retained` is a retained-index
    /// list into `config.rows`, applied UNIFORMLY so index-alignment
    /// survives the filter. No synthesized id: `rows[i].id` IS the real
    /// exposed param id already, so writes dispatch through the
    /// byte-for-byte exposed-param path every other card uses.
    fn configure_from_filtered(&mut self, config: &ParamSurface, retained: &[usize]) {
        let n = retained.len();
        // Snapshot the previous id→index map and pushed-value cache BEFORE
        // the rebuild: the per-frame sync's dirty key carries over by row id
        // (an index that now belongs to a DIFFERENT param must not inherit
        // the old row's pushed value — it re-pushes via the NaN seed).
        let prev_index = std::mem::take(&mut self.row_id_index);
        let prev_pushed = std::mem::take(&mut self.last_pushed_values);
        self.resize(n);
        self.rows = retained.iter().map(|&i| config.rows[i].clone()).collect();
        self.row_host.index_swatches(&self.rows);
        self.current_values = retained.iter().map(|&i| config.rows[i].value.base).collect();
        self.last_pushed_values = self
            .rows
            .iter()
            .map(|row| prev_index.get(row.id.as_ref()).map(|&j| prev_pushed[j]).unwrap_or(f32::NAN))
            .collect();

        // BUG-313: rebuild the id→local-row-index join map from the retained
        // rows. The per-frame value sync joins the full manifest against this
        // by id — no positional retained-index list. A duplicate id would
        // silently corrupt the join (last-wins); assert against it here.
        self.row_id_index.clear();
        self.row_id_index.reserve(n);
        for (i, row) in self.rows.iter().enumerate() {
            let prev = self.row_id_index.insert(row.id.to_string(), i);
            debug_assert!(
                prev.is_none(),
                "BUG-313: duplicate param id {:?} in scene properties rows",
                row.id,
            );
        }

        let mods: Vec<RowMod> = retained.iter().map(|&i| config.rows[i].modulation.clone()).collect();
        self.mod_state.sync_from_config(n, &mods);

        // Retain audio facts with their rows so filtering cannot drift the
        // audio state away from the visible parameter.
        self.mod_state.sync_audio(
            self.rows.iter().map(|row| row.audio.clone()),
            &config.audio_sends,
        );
    }

    fn restore_live(&mut self, target: &GraphParamTarget) {
        let ctx = RowInteraction {
            target,
            rows: &mut self.rows,
            modulation: &mut self.mod_state,
            values: &mut self.current_values,
            row_indices: &self.row_id_index,
        };
        self.row_host.restore_live(ctx);
    }

    fn handle_pointer_down(
        &mut self,
        node: NodeId,
        pos: Vec2,
        tree: &mut UITree,
        target: &GraphParamTarget,
    ) -> Vec<PanelAction> {
        let ctx = RowInteraction {
            target,
            rows: &mut self.rows,
            modulation: &mut self.mod_state,
            values: &mut self.current_values,
            row_indices: &self.row_id_index,
        };
        self.row_host.handle_pointer_down(node, pos, tree, ctx)
    }

    fn handle_drag(
        &mut self,
        pos: Vec2,
        tree: &mut UITree,
        fine: bool,
        target: &GraphParamTarget,
    ) -> Vec<PanelAction> {
        let ctx = RowInteraction {
            target,
            rows: &mut self.rows,
            modulation: &mut self.mod_state,
            values: &mut self.current_values,
            row_indices: &self.row_id_index,
        };
        self.row_host.handle_drag(pos, tree, fine, ctx)
    }
}

/// Placeholder `ParamRow` used only to size `SceneCardState::resize`'s
/// grow step before the real per-row info is written by the build pass —
/// never observed by a click/drag (every live index is overwritten before
/// `build_nodes` returns).
fn placeholder_param_info() -> ParamRow {
    ParamRow {
        id: manifold_foundation::ParamId::from(""),
        spec: RowSpec {
            tooltip: None,
            name: String::new(),
            min: 0.0,
            max: 1.0,
            default: 0.0,
            whole_numbers: false,
            is_angle: false,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: None,
            unit: None,
            section: None,
        disabled: None,
        material_role: None,
        inactive_reason: None,
        setup_warning: None,
        scene: None,
        limit: Default::default(),
        },
        value: crate::param_surface::RowValue { base: 0.0, effective: 0.0, exposed: false, driven: false },
        audio: AudioRowState::default(),
        clip_trigger: None,
        modulation: RowMod::default(),
        mapping: RowMapping {
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
        },
        rgb_members: None,
        modulator_stored: false,
    }
}

/// What a selection note compares: the layer, its active item and its
/// selected set (`None` when it follows the active item).
type NotedSelection = (LayerId, Option<SceneSelection>, Option<Vec<SceneSelection>>);

/// One numeric row's interactive node ids, set by `build_numeric_row` when
pub struct ScenePanel {
    open: bool,
    state: SceneSetupState,
    panel_w: f32,
    host: ChromeHost,
    /// The settings pane's scroll column; the tree pane's lives in `panes`.
    scroll: ScrollContainer,
    panes: panes::ScenePanes,
    reveal_properties: bool,
    pub(crate) content_parent: NodeId,
    bg_id: NodeId,
    add_environment_id: Option<NodeId>,
    /// Lighting's HDRI file row and its string param index.
    hdri_file_id: Option<(NodeId, usize)>,
    add_fog_id: Option<NodeId>,
    camera_setup_id: Option<NodeId>,
    new_scene_id: Option<NodeId>,
    open_graph_editor_id: Option<NodeId>,
    show_gizmos_id: Option<NodeId>,
    /// The global "Show gizmos" switch: the scene the panel shows draws its
    /// grid, light markers and physics shapes over the main output preview.
    /// UI-only workspace state, never saved with the project.
    show_gizmos: bool,
    /// P2 slice 2a (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the ONE
    /// unified properties card — the selected outliner item's rows, filtered
    /// straight off `full_params` (the layer's REAL generator
    /// `ParamSurface`) by section, rendered and dispatched through the same
    /// `build_param_row`/`RowIndex`/`row_action` core every effect/generator
    /// card row uses (P2 slice 2b deleted the five per-family
    /// `world_card`/`object_card`/`light_card`/`camera_card`/`modifier_card`
    /// fields this replaced — they had stopped rendering rows in 2a and were
    /// kept declared only to back a synthesized-id lookup that always missed).
    properties_card: SceneCardState,
    /// Shared parameter cards for the selected object's modifier chain. The
    /// cards are retained by stable object/modifier address across snapshots;
    /// only the selected object's live nodes are built this frame.
    object_modifier_cards: Vec<ParamCardPanel>,
    object_modifier_drag: Option<object_modifiers::ObjectModifierDrag>,
    object_modifier_drag_indicator: Option<NodeId>,
    selected_object_modifier: Option<ObjectModifierCardInfo>,
    object_modifier_pressed_card: Option<usize>,
    /// Shared parameter cards for force recipes in the selected scene.
    force_cards: Vec<ParamCardPanel>,
    force_pressed_card: Option<(LayerId, FoundationNodeId)>,
    /// A clip response reveal queued for the next structural build. The
    /// address is shared by force cards, object modifiers, and scene rows.
    pending_parameter_reveal: Option<(crate::view::UiGraphTarget, manifold_foundation::ParamId)>,
    /// Keep an explicitly opened property visible until selection changes.
    revealed_property: Option<(crate::view::UiGraphTarget, manifold_foundation::ParamId)>,
    /// P2 slice 2a: the scene panel's bound layer's FULL generator
    /// `ParamSurface` (every exposed param, every section) — built by
    /// `state_sync` the SAME way the main inspector's generator card is
    /// (`gen_params_to_surface`), for THIS panel's layer specifically (never
    /// `active_layer` — see `configure_params`'s doc comment). The
    /// properties body filters this down to the selected item's sections at
    /// build time.
    full_params: Option<ParamSurface>,
    full_param_id_index: ahash::AHashMap<String, usize>,
    /// The tree groups' add buttons: Objects opens the Objects menu, Lights
    /// adds a light, Forces opens the force picker.
    add_object_id: Option<NodeId>,
    add_light_id: Option<NodeId>,
    add_force_id: Option<NodeId>,
    /// The empty Physics group's action: Enable Physics on this object index.
    physics_enable_id: Option<(NodeId, SceneNodeRef)>,
    /// The groups the tree drew this frame, in order.
    tree_groups: Vec<SceneGroup>,
    header_buttons: item_header::HeaderButtons,
    copy_count: copies::CopyCount,
    /// P5 (D7): the outliner selection, per layer — UI-local workspace
    /// state, like fold state, NEVER serialized. Missing entry = the
    /// default (first object, else World) — resolved by
    /// `Self::resolve_selection`, which also handles the "selected id no
    /// longer exists after a graph edit" fallback.
    /// `LayerId` has no `Ord` impl (only `Hash`/`Eq`), so `HashMap` — not a
    /// `BTreeMap` — is the map that actually compiles; same "keyed per
    /// layer, UI-local, never serialized" contract either way.
    selection: std::collections::HashMap<LayerId, SceneSelection>,
    /// The selected items per layer, as Blender keeps a selection beside
    /// its active object: `selection` is the active item whose card shows,
    /// this the set the outliner highlights and the gizmos handle. A layer
    /// with no entry has its active item alone selected; an empty entry has
    /// nothing selected (a click on empty space), its card still showing.
    selected: std::collections::HashMap<LayerId, Vec<SceneSelection>>,
    /// UI_INVALIDATION D14 for the Scene panel: moves when a noted selection
    /// differs from the last noted one, or the selection write cleared a
    /// reveal or modifier focus. Noted at the gesture's commit (click, menu,
    /// external pick), never on press, so a press does not rebuild under a
    /// drag.
    selection_version: u64,
    noted_selection: Option<NotedSelection>,
    /// Moves on a body scroll or a card tween frame: the planner rebuilds
    /// the inspector region the docked body lives in (UI_INVALIDATION D6).
    view_version: u64,
    focus_cleared: bool,
    /// Every outliner row's click target this frame — `(node_id, what
    /// selecting it means)`.
    outliner_row_ids: Vec<(NodeId, SceneSelection)>,
    /// Every object row's eye toggle this frame — `(node_id, the object's
    /// current `visible` RowValue)`. A click flips the value (writes
    /// `!(value > 0.5)` as 0.0/1.0) through the same
    /// `SceneSetupParamChanged` fourth-surface path every other row uses.
    outliner_eye_ids: Vec<(NodeId, RowValue)>,
    /// Every selected Water-family child properties header Hide button this
    /// frame — `(node_id, the child's visible write row)`. The action always
    /// writes zero; it never removes the child graph output.
    object_hide_ids: Vec<(NodeId, RowValue)>,
    /// `(identity_node_id, name_label_node_id, current_name)` for the
    /// properties header's editable name row, when a Known object is
    /// selected this frame (`object_node_id`, the exact address
    /// `RenameSceneObjectCommand` takes) — resolves a name-label click to its
    /// rename action, and backs
    /// `object_name_rect` (the app's text-input anchor lookup). At most one
    /// entry per frame (P5: one selection, one properties header).
    object_name_ids: Vec<(SceneNodeRef, NodeId, String)>,
    /// BUG-193/P5: `(remove_button_node_id, index)` for the properties
    /// header's "Remove" button, when a Known object is selected this frame
    /// — resolves to `PanelAction::SceneSetupRemoveObject`. At most one
    /// entry per frame.
    object_remove_ids: Vec<(NodeId, SceneNodeRef)>,
    /// P5 (D11): `(duplicate_button_node_id, index)` for the properties
    /// header's "Duplicate" button, when a Known object is selected this
    /// frame — resolves to `PanelAction::SceneSetupDuplicateObject`.
    object_duplicate_ids: Vec<(NodeId, SceneNodeRef)>,
    /// Child-only remove/duplicate targets. Physical indices are not unique
    /// across a virtual group parent and its first child, so these have their
    /// own routing tables and stable widget keys use the object node id.
    submesh_remove_ids: Vec<(NodeId, SceneNodeRef)>,
    submesh_duplicate_ids: Vec<(NodeId, SceneNodeRef)>,
    /// scene-panel-ux lane: `(frame_button_node_id, object)` for the properties
    /// header's "Frame" button, when a Known object is selected this frame
    /// — resolves to `PanelAction::SceneSetupFrameSelected`.
    object_frame_ids: Vec<(NodeId, SceneNodeRef)>,
    object_enable_physics_ids: Vec<(NodeId, SceneNodeRef)>,
    object_disable_physics_ids: Vec<(NodeId, SceneNodeRef)>,
    object_fluid_role_ids: Vec<(NodeId, SceneNodeRef)>,
    fluid_role_target_ids: Vec<(NodeId, SceneNodeRef)>,
    fluid_role_remove_ids: Vec<(NodeId, SceneNodeRef)>,
    /// scene-panel-ux lane: fold state for properties sections, keyed by
    /// section NAME globally within the panel (folding "Material" folds it
    /// for every object). UI-local, never serialized. Missing entry = expanded.
    section_folded: ahash::AHashMap<String, bool>,
    /// Fold state for tree groups. UI-local, never serialized. Missing entry =
    /// expanded.
    outliner_folded: ahash::AHashMap<SceneGroup, bool>,
    /// Expanded compound groups, keyed per bound layer and kept in UI state.
    /// Widget-key ordinal of each object row, so two rows never share a key
    /// range. Ordinals are the lowest free numbers, kept while the row lives.
    object_key_ordinals: ahash::AHashMap<SceneNodeRef, usize>,
    expanded_groups: std::collections::HashMap<LayerId, ahash::AHashSet<SceneNodeRef>>,
    group_toggle_ids: Vec<(NodeId, SceneNodeRef)>,
    /// UX-P2 (D6): `(button_node_id, group_node_id)` for the single "+ Add
    /// Modifier" button built this frame, when the selected object's chain
    /// is addable (was `modifier_add_ids: Vec<(NodeId, u32, String)>`, one
    /// entry per chip — the click now opens the shared dropdown instead of
    /// resolving directly, so there's at most one entry and no `type_id`).
    add_modifier_button_id: Option<(NodeId, SceneNodeRef)>,
    /// P4b: `(button_node_id, scene_object_id, SkinRowVm)` for the Skin row's
    /// source picker. The click opens the shared dropdown; selection dispatches
    /// `SceneSetupSkinSourceSet`.
    skin_source_ids: Vec<(NodeId, u32, SkinRowVm)>,
    /// P4b: `(button_node_id, scene_object_id, SkinRowVm)` for the Skin row's
    /// target-map picker.
    skin_target_ids: Vec<(NodeId, u32, SkinRowVm)>,
    /// BUG-193/P5: `(remove_button_node_id, index)` for the properties
    /// header's "Remove" button, when a Known light is selected this frame —
    /// resolves to `PanelAction::SceneSetupRemoveLight`. At most one entry
    /// per frame.
    light_remove_ids: Vec<(NodeId, SceneNodeRef)>,
    /// P5: `(light_node_doc_id, name_label_node_id, current_name)` for the
    /// properties header's editable light name row, when a Known light is
    /// selected this frame — mirrors `object_name_ids`, backs
    /// `light_name_rect`.
    light_name_ids: Vec<(SceneNodeRef, NodeId, String)>,
    /// Feature presence is captured on selection, never inferred during a drag.
    material_mode_ids: Vec<(NodeId, usize)>,
    material_feature_context: Option<(LayerId, SceneNodeRef, SceneNodeRef)>,
    material_visible_features: ahash::AHashSet<crate::param_surface::MaterialFeature>,
    material_feature_order: Vec<crate::param_surface::MaterialFeature>,
    material_remove_ids: Vec<(NodeId, crate::param_surface::MaterialFeature, manifold_foundation::ParamId)>,
    /// The Material section's preset and add-feature menu buttons.
    material_menus: Vec<(NodeId, material_menus::MaterialMenu)>,
    active_material_info: Option<MaterialInspectorInfo>,
    /// The current properties pass's per-light sub-sections, in light
    /// order; empty outside Lighting.
    property_lights: Vec<lighting::PropertyLight>,
    /// How many of `property_lights` have their header drawn this pass.
    property_lights_drawn: usize,
    /// The current properties pass's group for undescribed controls.
    property_fallback_group: ControlGroup,
    /// Where the current pass is in the material block: 0 not drawn, 1 its
    /// header drawn, 2 closed with its actions.
    material_block_stage: u8,
    panel_rect: Rect,
    /// Scrollable properties viewport used to validate object-card gestures.
    /// The title bar and neighbouring surfaces are outside this region.
    object_modifier_bounds: Rect,
    object_cards_animating: bool,
    object_cards_were_animating: bool,
    /// Reused per frame: the drawer reflows the object cards report.
    reflow_scratch: Vec<crate::scroll_container::Reflow>,
    /// Object-card motion moved built nodes since the frame last asked.
    moved_in_place: bool,
}

impl Default for ScenePanel {
    fn default() -> Self {
        Self {
            open: false,
            state: SceneSetupState::default(),
            panel_w: PANEL_W_MIN,
            host: ChromeHost::new(),
            scroll: ScrollContainer::new(),
            panes: Default::default(),
            reveal_properties: false,
            content_parent: NodeId::PLACEHOLDER,
            bg_id: NodeId::PLACEHOLDER,
            add_environment_id: None,
            hdri_file_id: None,
            add_fog_id: None,
            camera_setup_id: None,
            new_scene_id: None,
            open_graph_editor_id: None,
            show_gizmos_id: None,
            show_gizmos: false,
            properties_card: SceneCardState::new(),
            object_modifier_cards: Vec::new(),
            object_modifier_drag: None,
            object_modifier_drag_indicator: None,
            selected_object_modifier: None,
            object_modifier_pressed_card: None,
            force_cards: Vec::new(),
            force_pressed_card: None,
            pending_parameter_reveal: None,
            revealed_property: None,
            full_params: None,
            full_param_id_index: ahash::AHashMap::new(),
            add_object_id: None,
            add_light_id: None,
            add_force_id: None,
            physics_enable_id: None,
            tree_groups: Vec::new(),
            header_buttons: Default::default(),
            copy_count: Default::default(),
            selection: std::collections::HashMap::new(),
            selected: std::collections::HashMap::new(),
            selection_version: 0,
            noted_selection: None,
            view_version: 0,
            focus_cleared: false,
            outliner_row_ids: Vec::new(),
            outliner_eye_ids: Vec::new(),
            object_hide_ids: Vec::new(),
            object_name_ids: Vec::new(),
            object_remove_ids: Vec::new(),
            object_duplicate_ids: Vec::new(),
            submesh_remove_ids: Vec::new(),
            submesh_duplicate_ids: Vec::new(),
            object_frame_ids: Vec::new(),
            object_enable_physics_ids: Vec::new(),
            object_disable_physics_ids: Vec::new(),
            object_fluid_role_ids: Vec::new(),
            fluid_role_target_ids: Vec::new(),
            fluid_role_remove_ids: Vec::new(),
            section_folded: ahash::AHashMap::new(),
            outliner_folded: ahash::AHashMap::new(),
            object_key_ordinals: ahash::AHashMap::new(),
            expanded_groups: std::collections::HashMap::new(),
            group_toggle_ids: Vec::new(),
            add_modifier_button_id: None,
            skin_source_ids: Vec::new(),
            skin_target_ids: Vec::new(),
            light_remove_ids: Vec::new(),
            light_name_ids: Vec::new(),
            material_mode_ids: Vec::new(),
            material_feature_context: None,
            material_visible_features: ahash::AHashSet::new(),
            material_feature_order: Vec::new(),
            material_remove_ids: Vec::new(),
            material_menus: Vec::new(),
            active_material_info: None,
            property_lights: Vec::new(),
            property_lights_drawn: 0,
            property_fallback_group: ControlGroup::Transform,
            material_block_stage: 0,
            panel_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            object_modifier_bounds: Rect::new(0.0, 0.0, 0.0, 0.0),
            object_cards_animating: false,
            object_cards_were_animating: false,
            reflow_scratch: Vec::new(),
            moved_in_place: false,
        }
    }
}

impl ScenePanel {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Node-intent dispatch for this panel's right-click gestures: the
    /// properties card's slider resets (main rows + armed drawers). Called
    /// from `UIRoot::repopulate_intents` like every other intent-bearing
    /// panel — the missing hookup was why scene-panel sliders had no
    /// right-click reset while every other slider in the app did.
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        let (true, SceneSetupState::Live(vm)) = (self.open, &self.state) else {
            return;
        };
        self.properties_card.row_host.register_intents(intents);
        // The one param menu, addressed at this panel's own layer (BUG-292).
        let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
        self.properties_card
            .row_host
            .register_param_menus(&self.properties_card.rows, &target, intents);
        for card in &self.object_modifier_cards {
            card.register_intents(intents);
        }
        for card in &self.force_cards {
            card.register_intents(intents);
        }
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn toggle(&mut self) {
        self.open = !self.open;
    }

    pub fn close(&mut self) {
        self.open = false;
    }

    /// Update the data the panel renders. Called from `state_sync` on a
    /// structural sync while the panel is open (or about to become open) —
    /// rebuilt fresh from the snapshot every time (D1: "no rotting, no
    /// staleness").
    pub fn configure(&mut self, state: SceneSetupState) {
        self.state = state;
        self.assign_object_key_ordinals();
        self.clear_stale_parameter_reveal();
        if !matches!(self.state, SceneSetupState::Live(_)) { self.clear_force_cards(); }
        self.rebuild_object_modifier_cards_from_projection();
    }

    /// Give every live object row its widget-key ordinal: rows keep theirs
    /// while they live, new rows take the lowest free number, and rows that
    /// left free theirs. Document ids repeat across import groups, so the
    /// key ranges cannot come from them.
    fn assign_object_key_ordinals(&mut self) {
        let SceneSetupState::Live(vm) = &self.state else {
            self.object_key_ordinals.clear();
            return;
        };
        let live: Vec<&SceneNodeRef> = vm
            .objects
            .iter()
            .filter_map(|object| match object {
                ObjectRowVm::Known(row) => Some(&row.node),
                ObjectRowVm::Custom { .. } => None,
            })
            .collect();
        self.object_key_ordinals.retain(|node, _| live.contains(&node));
        for node in live {
            if self.object_key_ordinals.contains_key(node) {
                continue;
            }
            let taken: ahash::AHashSet<usize> = self.object_key_ordinals.values().copied().collect();
            let ordinal = (0..).find(|candidate| !taken.contains(candidate)).unwrap_or(0);
            self.object_key_ordinals.insert(node.clone(), ordinal);
        }
    }

    /// The key-range number of `node`'s widgets.
    pub(crate) fn row_key(&self, node: &SceneNodeRef) -> usize {
        self.object_key_ordinals.get(node).copied().unwrap_or(0)
    }

    /// P2 slice 2a: hand the panel the layer's FULL generator
    /// `ParamSurface` (state_sync builds it via the SAME
    /// `gen_params_to_surface` the main inspector's generator card uses, for
    /// THIS panel's bound layer — `live_layer_id()`, never `active_layer`:
    /// the panel edits the scene of its OWN docked layer, which can
    /// legitimately differ from the app's active layer (BUG-292). The
    /// properties body filters this down to the selected item's controls at
    /// build time — see `build_item_properties`.
    pub fn configure_params(&mut self, config: Option<ParamSurface>) {
        self.full_param_id_index.clear();
        if let Some(surface) = config.as_ref() {
            self.full_param_id_index.reserve(surface.rows.len());
            for (index, row) in surface.rows.iter().enumerate() {
                self.full_param_id_index.insert(row.id.to_string(), index);
            }
        }
        self.full_params = config;
        self.clear_stale_parameter_reveal();
        self.rebuild_object_modifier_cards_from_projection();
    }

    /// Build the panel as a docked column into `rect`
    /// The layer this panel is live on, if it's showing the full panel.
    /// The app's per-frame value sync uses this to resolve the layer's
    /// generator graph without re-deriving the selection.
    pub fn live_layer_id(&self) -> Option<&LayerId> {
        match &self.state {
            SceneSetupState::Live(vm) => Some(&vm.layer_id),
            _ => None,
        }
    }

    /// The scene layer whose gizmos draw over the output preview: the one
    /// this panel shows, while "Show gizmos" is on.
    pub fn gizmo_layer(&self) -> Option<&LayerId> {
        self.live_layer_id().filter(|_| self.show_gizmos)
    }

    /// Turn "Show gizmos" off: perform mode forces it off (D6), so a show
    /// never starts with editor chrome armed over the preview.
    pub fn hide_gizmos(&mut self) {
        if self.show_gizmos {
            self.show_gizmos = false;
            self.view_version += 1;
        }
    }

    /// The active item of the scene this panel shows: the one whose card
    /// shows.
    pub fn live_selection(&self) -> Option<&SceneSelection> {
        self.selection.get(self.live_layer_id()?)
    }

    /// Every selected item of the scene this panel shows, in selection
    /// order, the active one last when it is selected.
    pub fn live_selected(&self) -> &[SceneSelection] {
        self.live_layer_id().map_or(&[], |layer| self.selected_of(layer))
    }

    fn selected_of(&self, layer: &LayerId) -> &[SceneSelection] {
        match self.selected.get(layer) {
            Some(set) => set,
            None => self.selection.get(layer).map_or(&[], std::slice::from_ref),
        }
    }

    /// Build into the inspector's Scene tab content rect
    /// (`InspectorCompositePanel::scene_content_rect`). No-op when closed.
    pub fn build_docked(&mut self, tree: &mut UITree, rect: Rect) {
        if !self.open {
            return;
        }
        self.panel_w = rect.width.max(PANEL_W_MIN);
        self.build_nodes(tree, rect.x, rect.y, rect.height);
    }

    fn chrome_view(&self) -> View {
        View::panel()
            .fill()
            .style(UIStyle {
                bg_color: Color32::new(19, 19, 22, 250),
                border_color: Color32::new(48, 48, 52, 255),
                border_width: 1.0,
                corner_radius: color::POPUP_RADIUS,
                ..UIStyle::default()
            })
            .interactive()
            .inert()
            .key(KEY_BG)
    }

    fn build_nodes(&mut self, tree: &mut UITree, x: f32, y: f32, panel_h: f32) {
        let chrome = self.chrome_view();
        self.host.build(tree, &chrome, Rect::new(x, y, self.panel_w, panel_h));
        self.bg_id = self.host.node_id_for_key(KEY_BG).unwrap_or(NodeId::PLACEHOLDER);
        self.panel_rect = Rect::new(x, y, self.panel_w, panel_h);
        // Reset every dynamic control id — repopulated by whichever
        // `build_*` branch below actually builds this frame (state_sync
        // rebuilds fresh every pass, D1 "no staleness").
        self.add_environment_id = None;
        self.hdri_file_id = None;
        self.add_fog_id = None;
        self.camera_setup_id = None;
        self.new_scene_id = None;
        self.open_graph_editor_id = None;
        self.show_gizmos_id = None;
        self.add_object_id = None;
        self.add_light_id = None;
        self.add_force_id = None;
        self.physics_enable_id = None;
        self.tree_groups.clear();
        self.header_buttons.clear();
        self.copy_count.clear();
        self.outliner_row_ids.clear();
        self.group_toggle_ids.clear();
        self.outliner_eye_ids.clear();
        self.object_hide_ids.clear();
        self.object_name_ids.clear();
        self.object_remove_ids.clear();
        self.object_duplicate_ids.clear();
        self.submesh_remove_ids.clear();
        self.submesh_duplicate_ids.clear();
        self.object_enable_physics_ids.clear();
        self.object_disable_physics_ids.clear();
        self.object_fluid_role_ids.clear();
        self.fluid_role_target_ids.clear();
        self.fluid_role_remove_ids.clear();
        self.add_modifier_button_id = None;
        self.skin_source_ids.clear();
        self.skin_target_ids.clear();
        self.light_remove_ids.clear();
        self.light_name_ids.clear();
        self.material_mode_ids.clear();
        self.material_menus.clear();
        self.material_remove_ids.clear();
        // Object modifier cards are retained for collapse/gesture state, but
        // their widget ids are frame-local. Clear them before rebuilding so
        // cards hidden by a non-object selection cannot claim a fresh row's
        // node range during event routing.
        for card in &mut self.object_modifier_cards {
            card.clear_nodes();
        }
        for card in &mut self.force_cards {
            card.clear_nodes();
        }
        // Same for the properties card: a Force, folded-outliner or missing
        // item branch skips `build_item_properties`, and its old ids would
        // otherwise register intents on whatever now sits in those slots.
        self.properties_card.clear_nodes();
        self.active_material_info = None;
        let body = Rect::new(x, y + PAD, self.panel_w, (panel_h - PAD * 2.0).max(0.0));
        self.build_body(tree, body);
        self.reveal_pending_parameter_response(tree);
        self.rebuild_object_modifier_drag_overlay(tree);
    }

    fn build_sentence(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, cy: f32, sentence: &str) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H * 2.0, sentence, wrapped_label_style());
        cy + ROW_H * 2.0 + ROW_GAP
    }

    fn build_no_generator(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32) -> f32 {
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "This layer has no 3D scene yet.",
            label_style(),
        );
        cy += ROW_H + ROW_GAP * 2.0;
        self.new_scene_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(),
            "New 3D Scene",
            KEY_NEW_SCENE,
        ));
        cy + ROW_H + ROW_GAP
    }

    fn build_no_scene(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32) -> f32 {
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "This generator has no 3D scene.",
            label_style(),
        );
        cy += ROW_H + ROW_GAP * 2.0;
        self.open_graph_editor_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(),
            "Open Graph Editor",
            KEY_OPEN_GRAPH_EDITOR,
        ));
        cy + ROW_H + ROW_GAP
    }

    /// External selection write (REALTIME_3D_DESIGN.md P6): a viewport
    /// object-pick sets the SAME UI-local `self.selection` map an outliner
    /// row click does (`handle_event`'s `SceneSetupSelectionChanged` arm) —
    /// one selection store for the whole app, not a second one that could
    /// drift from the panel's own. A change moves the selection version, which
    /// rebuilds the Properties section.
    pub fn set_selection(&mut self, layer_id: LayerId, sel: SceneSelection) {
        self.clear_item_focus();
        self.selection.insert(layer_id.clone(), sel.shown());
        self.selected.remove(&layer_id);
        self.reveal_properties = true;
        self.note_selection(&layer_id);
    }

    /// External selection write of a whole set (a Shift-click on a gizmo):
    /// `items` selected, the last one active. An empty set clears the
    /// selection and keeps the active item's card showing, as Blender does.
    pub fn select_items(&mut self, layer_id: LayerId, items: Vec<SceneSelection>) {
        let items: Vec<SceneSelection> = items.into_iter().map(SceneSelection::shown).collect();
        match items.last().cloned() {
            None => {
                self.selected.insert(layer_id.clone(), items);
                self.note_selection(&layer_id);
            }
            Some(active) => {
                if self.selection.get(&layer_id) != Some(&active) {
                    self.clear_item_focus();
                    self.reveal_properties = true;
                }
                self.selection.insert(layer_id.clone(), active);
                if items.len() == 1 {
                    self.selected.remove(&layer_id);
                } else {
                    self.selected.insert(layer_id.clone(), items);
                }
                self.note_selection(&layer_id);
            }
        }
    }

    /// Nothing selected on `layer_id` (a click on empty space); the active
    /// item's card keeps showing.
    pub fn clear_selected(&mut self, layer_id: LayerId) {
        self.select_items(layer_id, Vec::new());
    }

    /// A Shift-click on an outliner row: `sel` joins the selection as the
    /// active item, or leaves it when it was selected.
    fn toggle_selected(&mut self, layer_id: &LayerId, sel: SceneSelection) {
        let sel = sel.shown();
        let mut items = self.selected_of(layer_id).to_vec();
        let before = items.len();
        items.retain(|item| *item != sel);
        // When the active item leaves, the last one still selected shows.
        if items.len() == before {
            items.push(sel);
        }
        self.select_items(layer_id.clone(), items);
    }

    /// Unfold the tree group `parent` (its row's address), so a selected
    /// item under it shows.
    pub fn unfold(&mut self, layer_id: LayerId, parent: SceneNodeRef) {
        self.expanded_groups.entry(layer_id).or_default().insert(parent);
    }

    /// The Scene panel's selection version (UI_INVALIDATION D14): the
    /// planner re-runs the inspector projection when it moves.
    pub fn selection_version(&self) -> u64 {
        self.selection_version
    }

    /// A selection write drops any reveal and modifier focus; the next note
    /// counts that as a change when something was focused.
    fn clear_item_focus(&mut self) {
        if self.pending_parameter_reveal.is_some()
            || self.revealed_property.is_some()
            || self.selected_object_modifier.is_some()
        {
            self.focus_cleared = true;
        }
        self.pending_parameter_reveal = None;
        self.revealed_property = None;
        self.selected_object_modifier = None;
    }

    /// Compare-before-bump: the version moves only when `layer_id`'s
    /// selection differs from the last noted one, or a focus was cleared
    /// since the last note.
    pub(crate) fn note_selection(&mut self, layer_id: &LayerId) {
        let current = (layer_id.clone(), self.selection.get(layer_id).cloned(), self.selected.get(layer_id).cloned());
        let cleared = std::mem::take(&mut self.focus_cleared);
        if cleared || self.noted_selection.as_ref() != Some(&current) {
            self.noted_selection = Some(current);
            self.selection_version += 1;
        }
    }

    /// Capture the destination before a keyboard shortcut can overtake the
    /// queued click. Folding remains a click action, not a selection change.
    pub fn select_outliner_node(&mut self, node_id: NodeId) -> bool {
        let Some((_, selection)) = self.outliner_row_ids.iter().find(|(id, _)| *id == node_id) else {
            return false;
        };
        if matches!(selection, SceneSelection::OutlinerFold(_)) { return false; }
        let SceneSetupState::Live(vm) = &self.state else { return false; };
        let (layer_id, selection) = (vm.layer_id.clone(), selection.clone());
        self.clear_item_focus();
        self.selected.remove(&layer_id);
        self.selection.insert(layer_id, selection);
        self.view_version += 1;
        true
    }

    /// The current selection for `vm.layer_id`, resolving the D7 fallback
    /// (a dangling id after a graph edit, or no entry yet) to the first
    /// Known object, else World — and persisting the resolved value back
    /// into `self.selection` so a later `object_name_rect`/click lookup
    /// this same frame sees the same answer `build_outliner` used.
    fn resolve_selection(&mut self, vm: &SceneSetupVm) -> SceneSelection {
        let current = self.selection.get(&vm.layer_id).cloned().map(SceneSelection::shown);
        let resolved = match current {
            Some(sel) if Self::selection_exists(vm, sel.clone()) => sel,
            _ => Self::default_selection(vm),
        };
        // A selected item a graph edit removed leaves the selection.
        if let Some(set) = self.selected.get_mut(&vm.layer_id) {
            set.retain(|sel| Self::selection_exists(vm, sel.clone()));
        }
        // The build shows the fallback, so it is what the next note compares
        // against: reselecting the vanished item after a redo is a change.
        if let Some((layer, noted, noted_set)) = self.noted_selection.as_mut()
            && *layer == vm.layer_id
        {
            *noted = Some(resolved.clone());
            *noted_set = self.selected.get(&vm.layer_id).cloned();
        }
        self.selection.insert(vm.layer_id.clone(), resolved.clone());
        resolved
    }

    fn selection_exists(vm: &SceneSetupVm, sel: SceneSelection) -> bool {
        matches!(sel, SceneSelection::OutlinerFold(_)) || vm.items.iter().any(|item| item.selection == sel)
    }

    /// The first object, else World. A custom object row has no address, so
    /// it is listed but never selected.
    fn default_selection(vm: &SceneSetupVm) -> SceneSelection {
        vm.items
            .iter()
            .find(|item| matches!(item.selection, SceneSelection::Object(_)))
            .map_or(SceneSelection::World, |item| item.selection.clone())
    }

    /// The selected item's controls (D8): the rows it owns, in manifest
    /// order, grouped by control group in declaration order, one fold
    /// section per group, each group's switch first. Hidden rows are left
    /// out unless attached or holding a stored modulator, armed or not;
    /// rows the item's current mode does not read are left out unless
    /// attached (D6), and an attached unread row stays, dimmed with the
    /// reason.
    /// Material rows keep the material inspector's own sections inside the
    /// Material group, framed by its header and feature actions. Every row
    /// goes through the shared `build_param_row`/`RowHost` core, and
    /// `configure_from_filtered` builds the id join the per-frame value sync
    /// resolves against (BUG-313).
    pub(crate) fn build_item_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        rows: PropertyRows<'_>,
    ) -> f32 {
        let Some(mut config) = self.full_params.clone() else {
            self.properties_card.resize(0);
            return cy;
        };
        // BUG-292: rows dispatch to the panel's own bound layer.
        let Some(target) = self.live_layer_id().cloned().map(GraphParamTarget::GeneratorOf) else {
            self.properties_card.resize(0);
            return cy;
        };
        self.property_fallback_group = rows.fallback;
        let imported_selection = rows.object.is_some_and(|object| object.physics_imported);
        // The item's mode switches, read from their base values (D12).
        let modes: Vec<(Feature, u32)> = config
            .rows
            .iter()
            .filter(|row| rows.owned.iter().any(|id| id == row.id.as_ref()))
            .filter_map(|row| {
                let feature = row.spec.scene?.switch.filter(|feature| feature.has_modes())?;
                Some((feature, row.value.base.round().max(0.0) as u32))
            })
            .collect();
        let mut retained: Vec<usize> = Vec::new();
        for i in 0..config.rows.len() {
            let unread = match config.rows[i].spec.scene.and_then(|scene| scene.gate) {
                Some(Gate::Only(feature, read_in)) => modes
                    .iter()
                    .find(|(switch, _)| *switch == feature)
                    .is_some_and(|&(_, mode)| !read_in.contains(mode))
                    .then_some(feature),
                _ => None,
            };
            if let Some(feature) = unread
                && config.rows[i].is_attached()
            {
                config.rows[i].spec.inactive_reason = Some(feature.off_reason().into());
            }
            let p = &config.rows[i];
            let selected_material_id = p.spec.material_role.is_some()
                && self.active_material_info.as_ref().is_some_and(|info| {
                    info.params.iter().any(|(_, id)| id == &p.id)
                });
            let owned = selected_material_id || rows.owned.iter().any(|id| id == p.id.as_ref());
            let excluded = !selected_material_id && rows.excluded.iter().any(|id| id == p.id.as_ref());
            // A Hidden control shows while a modulator is stored on it, armed
            // or not: the row is the only way back to a disarmed one.
            let hidden_role = p.spec.scene.is_some_and(|scene| scene.role == SurfaceRole::Hidden)
                && !(p.modulator_stored || p.is_attached());
            let hidden = (hidden_role || (unread.is_some() && !p.is_attached()))
                && !self.property_explicitly_revealed(p);
            // Imported bodies keep their own collider shape; only they offer
            // the collider detail control.
            let wrong_body_kind = if imported_selection { p.spec.name == "Shape" } else { p.spec.name == "Collider Detail" };
            let shown = owned && !excluded && !hidden && !wrong_body_kind;
            if shown
                && self.material_param_selected(p)
                && (self.property_explicitly_revealed(p)
                    || (self.material_row_feature(p).is_none_or(|feature| self.material_feature_visible(&config.rows, feature))
                        && Self::material_panel_row_visible(p)))
            {
                retained.push(i);
            }
        }
        retained.sort_by_key(|&index| {
            let row = &config.rows[index];
            (
                self.property_section_rank(row),
                self.property_group(row) as usize,
                row.spec.scene.is_none_or(|scene| scene.switch.is_none()),
            )
        });
        // Material rows use the material inspector's stable buckets, inside
        // the Material group's positions.
        let material_positions: Vec<usize> = retained
            .iter()
            .enumerate()
            .filter_map(|(position, &index)| (config.rows[index].spec.material_role.is_some()
                || self.material_object_gain(&config.rows[index])).then_some(position))
            .collect();
        let mut material_rows: Vec<usize> = material_positions.iter().map(|&position| retained[position]).collect();
        material_rows.sort_by_key(|&index| {
            let row = &config.rows[index];
            (self.material_bucket(row), !matches!(row.spec.material_role, Some(MaterialParamRole::FeatureMode(_))))
        });
        for (position, index) in material_positions.into_iter().zip(material_rows) {
            retained[position] = index;
        }
        // Optional material features sit directly above their Add controls.
        retained.sort_by_key(|&index| {
            let row = &config.rows[index];
            (self.property_section_rank(row), self.property_group(row) as usize, self.material_row_feature(row).is_some())
        });
        self.properties_card.configure_from_filtered(&config, &retained);
        self.prepare_pending_parameter_properties();
        self.properties_card.restore_live(&target);

        let RowGeometry { label_width, slider_w } = super::param_card::row_geometry(inner_w, false);
        self.properties_card.row_host.section_header_ids.clear();
        let material = rows.object.filter(|object| object.material_inspector.is_some());
        self.material_block_stage = 0;
        let mut i = 0usize;
        while i < retained.len() {
            if let Some(light) = self.property_light_of(&config.rows[retained[i]]) {
                let run_end = (i..retained.len())
                    .find(|&j| self.property_light_of(&config.rows[retained[j]]) != Some(light))
                    .unwrap_or(retained.len());
                cy = self.build_light_headers_through(tree, (inner_x, inner_w), cy, light);
                for row in i..run_end {
                    cy = self.build_properties_block(tree, inner_x, cy, row, label_width, slider_w, target.clone());
                }
                i = run_end;
                continue;
            }
            let group = self.property_group(&config.rows[retained[i]]) as usize;
            if let Some(object) = material {
                cy = self.advance_material_block(tree, (inner_x, inner_w), cy, object, group);
            }
            let name = self.material_section_name(&config.rows[retained[i]]).unwrap_or_default();
            let feature = self.material_row_feature(&config.rows[retained[i]]);
            let remove_mode = feature.and_then(|feature| config.rows.iter().find(|row|
                self.material_param_selected(row)
                    && row.spec.material_role == Some(MaterialParamRole::FeatureMode(feature))
            ).map(|row| (feature, row.id.clone())));
            let header_w = if remove_mode.is_some() { inner_w - ROW_H - ROW_GAP } else { inner_w };
            let folded = self.material_section_folded(&name);
            self.section_folded.entry(name.clone()).or_insert(folded);
            let run_end = (i..retained.len())
                .find(|&j| self.material_section_name(&config.rows[retained[j]]).unwrap_or_default() != name)
                .unwrap_or(retained.len());
            let header_id = ParamCardPanel::build_section_header(
                tree,
                Some(self.content_parent),
                Rect::new(inner_x, cy, header_w, ROW_H),
                &name,
                folded,
                run_end - i,
                param_row_key_base(&config.rows[retained[i]].id),
            );
            tree.set_name(header_id, format!("scene_setup.group.{name}"));
            if config.rows[retained[i]].spec.material_role.is_some() {
                tree.set_name(header_id, format!("material.section.{}", config.rows[retained[i]].id));
            }
            self.properties_card.row_host.section_header_ids.push((header_id, name.clone()));
            self.properties_card.row_host.row_index.insert(tree.widget_of(header_id), i, RowRole::SectionHeader);
            if let Some((feature, mode_id)) = remove_mode {
                let remove = tree.add_button_keyed(
                    Some(self.content_parent), inner_x + inner_w - ROW_H, cy,
                    ROW_H, ROW_H, btn_style(), "×",
                    MATERIAL_LOOK_KEY_BASE + 64 + feature as u64,
                );
                tree.set_name(remove, format!("material.feature.remove.{mode_id}"));
                self.material_remove_ids.push((remove, feature, mode_id));
            }
            cy += ROW_H;
            if !folded {
                cy = self.build_material_section_sources(tree, inner_x, inner_w, cy, &name);
                for row in i..run_end {
                    cy = self.build_properties_block(tree, inner_x, cy, row, label_width, slider_w, target.clone());
                }
            }
            i = run_end;
        }
        if let Some(object) = material {
            cy = self.advance_material_block(tree, (inner_x, inner_w), cy, object, usize::MAX);
        }
        cy + ROW_GAP
    }

    /// The group a properties row sorts and titles under: material rows
    /// belong to Material, rows the scene table does not describe to the
    /// item's own first group.
    pub(super) fn property_group(&self, row: &ParamRow) -> ControlGroup {
        if row.spec.material_role.is_some() || self.material_object_gain(row) {
            return ControlGroup::Material;
        }
        row.spec.scene.map_or(self.property_fallback_group, |scene| scene.group)
    }

    /// Open the material block (look buttons) when the walk reaches the
    /// Material group, and close it (optional features, textures) once the
    /// walk passes it. `group` is the next row's group rank.
    fn advance_material_block(
        &mut self,
        tree: &mut UITree,
        (inner_x, inner_w): (f32, f32),
        mut cy: f32,
        object: &ObjectKnownRow,
        group: usize,
    ) -> f32 {
        let Some(info) = object.material_inspector.clone() else { return cy };
        let material = ControlGroup::Material as usize;
        if self.material_block_stage == 0 && group >= material {
            cy = self.build_material_header(tree, inner_x, inner_w, cy, &info);
            self.material_block_stage = 1;
        }
        if self.material_block_stage == 1 && group > material {
            cy = self.build_material_add_menu(tree, inner_x, inner_w, cy);
            cy = self.build_material_inspector(tree, inner_x, inner_w, cy, object, object.skin.as_ref());
            self.material_block_stage = 2;
        }
        cy
    }

    /// World: the World item's controls left once Lighting takes the
    /// environment (Fog, Ray Tracing), then the missing-fog row.
    fn build_world_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
        owned: &[String],
    ) -> f32 {
        cy = self.build_item_properties(tree, inner_x, inner_w, cy, PropertyRows::new(owned, ControlGroup::Fog));
        if matches!(vm.atmosphere, AtmosphereRowVm::None) {
            tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Fog: None", label_style());
            cy += ROW_H;
            self.add_fog_id = Some(tree.add_button_keyed(
                Some(self.content_parent), inner_x, cy, inner_w, ROW_H, btn_style(), "+ Add Fog", KEY_ADD_FOG,
            ));
            cy += ROW_H;
        }
        cy
    }

    /// UX-P2 (D6 of SCENE_PANEL_UX_DESIGN.md): the single "+ Add Modifier"
    /// button, replacing the old 7-chip grid (`build_add_modifier_row`).
    /// The click opens the shared `panels::dropdown` overlay, listing the
    /// SAME [`MESH_MODIFIER_CHOICES`] the chips used — resolved app-side
    /// (`UIRoot::try_open_dropdown_inner`) because the panel has no
    /// `&UITree` in `handle_event` to anchor the overlay itself, same
    /// resolve-at-open convention `SceneSetupEnumClicked` already uses.
    fn build_add_modifier_button(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        ordinal: usize,
        owner: SceneNodeRef,
    ) -> f32 {
        let btn_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(),
            "+ Add Modifier",
            modifier_add_button_key(ordinal),
        );
        self.add_modifier_button_id = Some((btn_id, owner));
        cy + ROW_H
    }

    /// The docked body's view version: scroll and card tween frames.
    pub fn view_version(&self) -> u64 {
        self.view_version
    }

    /// Whether a point lands inside the panel's own rect — for the app's
    /// drag-ownership dispatch (mirrors `AudioSetupPanel::point_in_panel`).
    pub fn point_in_panel(&self, pos: crate::node::Vec2) -> bool {
        self.open && self.panel_rect.contains(pos)
    }

    // UX-P2 (D3a)'s drag-armable value-cell cursor lookup (`value_cell_at`)
    // is DELETED — C-P1d converted Modifier (its last producer,
    // `object_value_cells`) onto the card row's own slider track, same as
    // every other family before it (`world_card`/`object_card`/`light_card`/
    // `camera_card`), so no family has a bespoke delta-drag value cell left.
    // `app.rs::update_cursor_for_position`'s Priority 2d block (the caller)
    // is deleted in the same commit.

    /// Handle one input event. Returns `(consumed, actions)`.
    pub fn handle_event(&mut self, event: &UIEvent, tree: &mut UITree) -> (bool, Vec<PanelAction>) {
        if !self.open {
            return (false, Vec::new());
        }
        if let Some(handled) = self.handle_pane_event(event) {
            return handled;
        }
        match event {
            UIEvent::RightClick { node_id: Some(node_id), .. } if self.select_outliner_node(*node_id) => {
                if let Some(layer_id) = self.state.as_live().map(|vm| vm.layer_id.clone()) {
                    self.note_selection(&layer_id);
                }
                (true, vec![PanelAction::Root(RootAction::SceneItemRightClicked)])
            }
            UIEvent::Click { node_id, modifiers, .. } => {
                if self.show_gizmos_id == Some(*node_id) {
                    self.show_gizmos = !self.show_gizmos;
                    self.view_version += 1;
                    return (true, Vec::new());
                }
                if let Some((_, group)) = self.group_toggle_ids.iter().find(|(id, _)| *id == *node_id).cloned() {
                    let Some(vm) = self.state.as_live() else {
                        return (true, Vec::new());
                    };
                    let groups = self.expanded_groups.entry(vm.layer_id.clone()).or_default();
                    if !groups.insert(group.clone()) {
                        groups.remove(&group);
                    }
                    return (true, vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]);
                }
                if let Some(actions) = self.object_modifier_card_click(*node_id, tree) {
                    return (true, actions);
                }
                if let Some(actions) = self.force_card_click(*node_id, tree) {
                    return (true, actions);
                }
                if let Some(actions) = self.copy_count_click(*node_id) {
                    return (true, actions);
                }
                // D7: an outliner row click sets the UI-local selection —
                // no command, no undo unit, valid even before a `Live` state
                // exists. D1 of SCENE_PANEL_UX_DESIGN.md: the noted
                // selection version re-projects Properties THIS frame
                // instead of waiting for the next unrelated sync.
                if let Some((_, sel)) = self.outliner_row_ids.iter().find(|(id, _)| *id == *node_id) {
                    // scene-panel-ux lane: handle outliner fold toggles
                    if let SceneSelection::OutlinerFold(group_name) = sel {
                        let folded = self.outliner_folded.entry(*group_name).or_insert(false);
                        *folded = !*folded;
                        return (true, vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]);
                    }
                    // Normal selection change; Shift adds the row to the
                    // selection or takes it out, as in Blender's outliner.
                    if let SceneSetupState::Live(vm) = &self.state {
                        let (layer_id, sel) = (vm.layer_id.clone(), sel.clone());
                        if modifiers.shift {
                            self.toggle_selected(&layer_id, sel);
                        } else {
                            self.clear_item_focus();
                            self.selection.insert(layer_id.clone(), sel);
                            self.selected.remove(&layer_id);
                            self.note_selection(&layer_id);
                        }
                        return (true, vec![PanelAction::Root(RootAction::SceneSetupSelectionChanged(layer_id))]);
                    }
                    return (true, Vec::new());
                }
                let mut actions = Vec::new();
                if let SceneSetupState::Live(vm) = &self.state {
                    if self.camera_setup_id == Some(*node_id) {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupPrepareCamera(
                            vm.layer_id.clone(),
                        )));
                    } else if let Some((_, feature, mode_id)) = self.material_remove_ids.iter()
                        .find(|(id, _, _)| *id == *node_id)
                    {
                        return (true, self.material_remove_action(
                            *feature, mode_id, GraphParamTarget::GeneratorOf(vm.layer_id.clone()),
                        ));
                    } else if let Some(menu) = self.material_menu_click(
                        *node_id,
                        GraphParamTarget::GeneratorOf(vm.layer_id.clone()),
                    ) {
                        return (true, menu);
                    } else if let Some((_, row_value)) =
                        self.outliner_eye_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        // The eye toggle: writes `scene_object.visible`
                        // through the SAME fourth-surface path every other
                        // row uses — the [0,1] threshold flips between 0.0
                        // and 1.0 (D3's on/off convention).
                        let new_value = if row_value.value > 0.5 { 0.0 } else { 1.0 };
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupParamChanged(
                            vm.layer_id.clone(),
                            row_value.addr.scope_path.clone(),
                            row_value.addr.node_doc_id,
                            row_value.addr.param_id.clone(),
                            new_value,
                        )));
                    } else if let Some((_, row_value)) =
                        self.object_hide_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        // Water-family child Hide is intentionally one-way:
                        // it writes the local visible parameter to zero even
                        // when the row is already hidden.
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupParamChanged(
                            vm.layer_id.clone(),
                            row_value.addr.scope_path.clone(),
                            row_value.addr.node_doc_id,
                            row_value.addr.param_id.clone(),
                            0.0,
                        )));
                    } else if let Some((_, object)) =
                        self.object_frame_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        // scene-panel-ux lane: handle Frame button click
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupFrameSelected(
                            vm.layer_id.clone(),
                            object.clone(),
                        )));
                    } else if let Some((_, object)) =
                        self.object_duplicate_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupDuplicateObject(
                            vm.layer_id.clone(),
                            object.clone(),
                            1,
                        )));
                    } else if let Some((_, part)) =
                        self.submesh_duplicate_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupDuplicateSubmesh(
                            vm.layer_id.clone(),
                            part.clone(),
                        )));
                    } else if let Some((_, object)) = self.object_enable_physics_ids.iter().find(|(id, _)| *id == *node_id) {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupEnablePhysics(
                            vm.layer_id.clone(), object.clone(),
                        )));
                    } else if let Some((_, object)) = self.object_fluid_role_ids.iter().find(|(id, _)| *id == *node_id) {
                        actions.push(PanelAction::Root(RootAction::SceneSetupFluidRoleClicked {
                            layer_id: vm.layer_id.clone(),
                            object: object.clone(),
                            domains: vm.fluid_domains.clone(),
                            button_node_id: *node_id,
                        }));
                    } else if let Some((_, source)) = self.fluid_role_target_ids.iter().find(|(id, _)| *id == *node_id) {
                        actions.push(PanelAction::Root(RootAction::SceneSetupFluidRoleTargetClicked {
                            layer_id: vm.layer_id.clone(), source: source.clone(),
                            domains: vm.fluid_domains.clone(), button_node_id: *node_id,
                        }));
                    } else if let Some((_, source)) = self.fluid_role_remove_ids.iter().find(|(id, _)| *id == *node_id) {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupRemoveFluidRole {
                            layer_id: vm.layer_id.clone(), source: source.clone(),
                        }));
                    } else if let Some((_, object)) = self.object_disable_physics_ids.iter().find(|(id, _)| *id == *node_id) {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupDisablePhysics(
                            vm.layer_id.clone(), object.clone(),
                        )));
                    } else if let Some((light, _, current_name)) =
                        self.light_name_ids.iter().find(|(_, id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Root(RootAction::SceneSetupRenameLightClicked(
                            vm.layer_id.clone(),
                            light.clone(),
                            current_name.clone(),
                        )));
                    }
                }
                if let SceneSetupState::Live(vm) = &self.state {
                    if self.add_environment_id == Some(*node_id) {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupAddEnvironment(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                        )));
                    } else if self.add_fog_id == Some(*node_id) {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupAddFog(vm.layer_id.clone(), vm.scene_root_node_id)));
                    } else if let Some(act) = self
                        .outliner_click(*node_id, vm)
                        .or_else(|| self.hdri_file_click(*node_id))
                        .or_else(|| self.item_header_click(*node_id, vm))
                    {
                        actions.push(act);
                    } else if let Some((object, _, current_name)) =
                        self.object_name_ids.iter().find(|(_, id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Root(RootAction::SceneSetupRenameObjectClicked(
                            vm.layer_id.clone(),
                            object.clone(),
                            current_name.clone(),
                        )));
                    } else if let Some((_, owner)) =
                        self.add_modifier_button_id.clone().filter(|(id, _)| *id == *node_id)
                    {
                        // UX-P2 (D6): the button doesn't resolve a choice
                        // itself — it asks the app to open the shared
                        // dropdown (`SceneSetupAddModifierClicked`), which
                        // lists `MESH_MODIFIER_CHOICES` and dispatches the
                        // SAME `SceneSetupAddModifier` each old chip did.
                        actions.push(PanelAction::Root(RootAction::SceneSetupAddModifierClicked(
                            vm.layer_id.clone(),
                            owner,
                            *node_id,
                        )));
                    } else if let Some((_, scene_object_id, skin)) =
                        self.skin_source_ids.iter().find(|(id, _, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Root(RootAction::SceneSetupSkinSourceClicked {
                            layer_id: vm.layer_id.clone(),
                            scope_path: skin.source_scope_path.clone(),
                            scene_object_id: *scene_object_id,
                            source_node_id: skin.source_node_id,
                            target_map: skin.target_map,
                            source_options: skin.source_options.clone(),
                            button_node_id: *node_id,
                        }));
                    } else if let Some((_, scene_object_id, skin)) =
                        self.skin_target_ids.iter().find(|(id, _, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Root(RootAction::SceneSetupSkinTargetMapClicked {
                            layer_id: vm.layer_id.clone(),
                            scope_path: skin.source_scope_path.clone(),
                            scene_object_id: *scene_object_id,
                            source_node_id: skin.source_node_id,
                            current_target_map: skin.target_map,
                            button_node_id: *node_id,
                        }));
                    } else if let Some((_, object)) =
                        self.object_remove_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupRemoveObject(
                            vm.layer_id.clone(),
                            object.clone(),
                        )));
                    } else if let Some((_, part)) =
                        self.submesh_remove_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupRemoveSubmesh(
                            vm.layer_id.clone(),
                            part.clone(),
                        )));
                    } else if let Some((_, light)) =
                        self.light_remove_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupRemoveLight(
                            vm.layer_id.clone(),
                            light.clone(),
                        )));
                    } else if let Some((row, role)) = self.properties_card.row_host.row_index.get(tree.widget_of(*node_id)) {
                        // P2 slice 2b (`docs/WIDGET_TREE_DESIGN.md` section 4/section 5b):
                        // the ONE unified properties card's D/E/A buttons +
                        // config drawers, routed through the same
                        // `RowIndex`/`row_action` core `ParamCardPanel` uses
                        // — the id-array scan this replaced is gone. BUG-292:
                        // targets `GeneratorOf(vm.layer_id)`, the panel's own
                        // bound layer, NOT the active-layer-resolved plain
                        // `Generator` — a scene row always lives on the layer
                        // its panel is docked to, which can differ from the
                        // app's active layer.
                        let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                        for mut action in self.properties_row_action(tree, row, role, *node_id, target) {
                            if let PanelAction::Root(RootAction::BeginDriverPeriodTextInput { anchor, value, .. }) = &mut action {
                                *anchor = tree.get_bounds(*node_id);
                                *value = self.properties_card.mod_state.driver_effective_period(row);
                            }
                            actions.push(action);
                        }
                    }
                }
                match &self.state {
                    SceneSetupState::NoGenerator { layer_id } if self.new_scene_id == Some(*node_id) => {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupNewScene(layer_id.clone())));
                    }
                    SceneSetupState::NoScene { layer_id } if self.open_graph_editor_id == Some(*node_id) => {
                        actions.push(PanelAction::Root(RootAction::SceneSetupOpenGraphEditor(layer_id.clone())));
                    }
                    _ => {}
                }
                (!actions.is_empty(), actions)
            }
            // P4 (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D8): double-click on a
            // drag-armable value cell opens its type-in box. The unified
            // properties card's rows resolve through the SAME shared
            // `RowHost::value_cell_typein` `ParamCardPanel` uses — target is
            // the panel's own bound layer (BUG-292), value is the row's
            // synced base, and the anchor rect is read from the live tree
            // (`handle_event` has `&UITree`; the old "no tree" comment was
            // stale). Emits `BeginParamTextInput`, the card's exact type-in
            // action, so the app's `InspectorParam` commit drives the same
            // `ValueRef::Param(GeneratorOf, ..)` scrub wire the drag uses.
            UIEvent::DoubleClick { node_id, .. } => {
                if let Some(action) = self.object_modifier_card_double_click(*node_id, tree) {
                    return (true, vec![action]);
                }
                if let Some(action) = self.force_card_double_click(*node_id, tree) {
                    return (true, vec![action]);
                }
                if let SceneSetupState::Live(vm) = &self.state {
                    let card = &self.properties_card;
                    let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                    if let Some(action) = card.row_host.value_cell_typein(
                        *node_id,
                        tree,
                        &card.rows,
                        &card.current_values,
                        target,
                    ) {
                        return (true, vec![action]);
                    }
                }
                (false, Vec::new())
            }
            UIEvent::PointerDown { node_id, pos, .. } => {
                if let Some(actions) = self.object_modifier_card_pointer_down(*node_id, *pos, tree) {
                    return (true, actions);
                }
                if let Some(actions) = self.force_card_pointer_down(*node_id, *pos, tree) {
                    return (true, actions);
                }
                if let SceneSetupState::Live(vm) = &self.state {
                    let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                    let was_dragging = self.properties_card.row_host.is_dragging();
                    let actions = self.properties_card.handle_pointer_down(
                        *node_id,
                        *pos,
                        tree,
                        &target,
                    );
                    let actions = self.rewrite_material_rgb_actions(actions);
                    if was_dragging
                        || self.properties_card.row_host.is_dragging()
                        || !actions.is_empty()
                    {
                        return (true, actions);
                    }
                }
                (self.owns_node(*node_id) || self.point_in_panel(*pos), Vec::new())
            }
            UIEvent::DragBegin { node_id, .. } => {
                if self.begin_object_modifier_drag(*node_id, tree) {
                    return (true, Vec::new());
                }
                if node_id.is_some_and(|node| self.force_card_index_for_node(tree, node).is_some())
                    && self.force_pressed_card.is_some()
                {
                    return (true, Vec::new());
                }
                (self.properties_card.row_host.is_dragging(), Vec::new())
            }
            UIEvent::Drag { pos, modifiers, .. } => {
                if self.object_modifier_drag.is_some() || self.object_modifier_pressed_card.is_some() {
                    self.update_object_modifier_drag(*pos, tree, self.object_modifier_bounds);
                    return (true, self.object_modifier_card_drag(*pos, tree, modifiers.shift));
                }
                if self.force_pressed_card.is_some() {
                    return (true, self.force_card_drag(*pos, tree, modifiers.shift));
                }
                if let SceneSetupState::Live(vm) = &self.state {
                    let target = GraphParamTarget::GeneratorOf(vm.layer_id.clone());
                    let was_dragging = self.properties_card.row_host.is_dragging();
                    let actions = self.properties_card.handle_drag(
                        *pos,
                        tree,
                        modifiers.shift,
                        &target,
                    );
                    let actions = self.rewrite_material_rgb_actions(actions);
                    if was_dragging || !actions.is_empty() {
                        return (true, actions);
                    }
                }
                (false, Vec::new())
            }
            UIEvent::DragEnd { pos, .. } | UIEvent::PointerUp { pos, .. } => {
                if self.object_modifier_drag.is_some() {
                    return (true, self.finish_object_modifier_drag(*pos, tree, self.object_modifier_bounds));
                }
                if self.object_modifier_pressed_card.is_some() {
                    return (true, self.end_object_modifier_card_gesture(tree));
                }
                if self.force_pressed_card.is_some() {
                    return (true, self.end_force_card_gesture(tree));
                }
                let was_dragging = self.properties_card.row_host.is_dragging();
                let actions = self.properties_card.row_host.handle_drag_end();
                let actions = self.rewrite_material_rgb_actions(actions);
                (was_dragging || !actions.is_empty(), actions)
            }
            _ => (false, Vec::new()),
        }
    }

    /// Route a resolved `(row, role)` hit on the unified properties card to
    /// its `PanelAction` by delegating to the shared [`RowHost::row_action`]
    /// — the same core `ParamCardPanel` uses (WIDGET_TREE_DESIGN section 4/5b).
    /// `target` is the caller's `GraphParamTarget::GeneratorOf(vm.layer_id)`
    /// (BUG-292), since `SceneCardState` has no `live_layer_id` of its own.
    fn owns_node(&self, node_id: NodeId) -> bool {
        node_id == self.bg_id
    }

    /// The name label's rect for `object`, if a row for it was built
    /// this frame — the app's text-input anchor lookup (mirrors
    /// `AudioSetupPanel::send_label_rect`).
    pub fn object_name_rect(&self, tree: &UITree, object: &SceneNodeRef) -> Option<Rect> {
        let (_, node_id, _) = self.object_name_ids.iter().find(|(row, _, _)| row == object)?;
        Some(tree.get_bounds(*node_id))
    }

    /// The light name label's rect for `light`, if the properties
    /// header was built for it this frame — mirrors `object_name_rect`.
    pub fn light_name_rect(&self, tree: &UITree, light: &SceneNodeRef) -> Option<Rect> {
        let (_, node_id, _) = self.light_name_ids.iter().find(|(row, _, _)| row == light)?;
        Some(tree.get_bounds(*node_id))
    }
}

fn scrollbar_style() -> ScrollbarStyle {
    ScrollbarStyle {
        track_color: color::SCROLLBAR_TRACK_C32,
        thumb_color: color::SCROLLBAR_THUMB_C32,
        thumb_hover_color: color::SCROLLBAR_THUMB_HOVER_C32,
        corner_radius: color::SMALL_RADIUS,
    }
}

pub(crate) fn btn_style() -> UIStyle {
    UIStyle { font_size: color::FONT_LABEL, ..crate::chrome::components::segment_style(false) }
}

pub(crate) fn label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(150, 150, 160, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn wrapped_label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(150, 150, 160, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn header_label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(224, 224, 228, 255),
        font_size: color::FONT_BODY,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

fn section_label_style() -> UIStyle {
    UIStyle {
        text_color: Color32::new(190, 190, 198, 255),
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        ..UIStyle::default()
    }
}

/// A drag-armable value label — visually distinct (subtle hover fill) from a
/// bare `label_style()` text row so it reads as draggable, not static prose
/// (the affordance-legibility rule: DESIGN_DOC_STANDARD section 5). UX-P2 (D3c):
/// `text_color` is the SAME `SLIDER_TEXT_C32` token the `BitmapSlider` value
/// box uses — `font_size`/`text_align` already matched (both `FONT_LABEL`/
/// `Center`) before this phase; token parity across the panel's two value
/// shapes (slider rows vs. drag-scrub cells) is the point, not a new style.
fn drag_value_style() -> UIStyle {
    UIStyle {
        bg_color: Color32::new(30, 30, 34, 200),
        hover_bg_color: Color32::new(44, 44, 50, 255),
        text_color: color::SLIDER_TEXT_C32,
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Center,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
    }
}

fn driven_label_style() -> UIStyle {
    UIStyle {
        text_color: color::TEXT_DIMMED,
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Right,
        ..UIStyle::default()
    }
}

#[cfg(test)]
#[path = "scene_setup_panel/tests.rs"]
mod tests;
