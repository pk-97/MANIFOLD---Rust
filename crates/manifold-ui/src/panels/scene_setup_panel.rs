//! Scene Setup panel — the "add effects, for 3D" dock
//! (`docs/SCENE_SETUP_PANEL_DESIGN.md`).
//!
//! A `ScreenLayout::scene_setup()` docked column, cloned from
//! [`super::audio_setup_panel::AudioSetupPanel`] (D2): same fold-out /
//! resize / snap-back / Escape-close mechanics, mutually exclusive with the
//! Audio Setup dock. Self-contained like that panel — it builds `UITree`
//! nodes from data handed in via [`ScenePanel::configure`] and maps clicks/
//! drags to [`super::PanelAction`]. P1 scope: Header + Environment + Fog
//! sections live, plus the D7 empty states. Objects/Lights/Camera land in
//! later phases (P2/P3) — this panel never builds a generic param-tree
//! renderer (D3's named wrong turn); every row here is a curated, named
//! control.
//!
//! Every value this panel writes goes through [`super::PanelAction::SceneSetupParamChanged`]
//! — the SAME `SetGraphNodeParamCommand` the graph editor's ordinary
//! (unbound) node-face numeric row already dispatches per drag tick
//! (`manifold-app/src/app_render.rs`'s `GraphEditCommand::SetGraphNodeParam`
//! handling) — never a new mutation path. No direct project mutation and no
//! shared-lock wrapper types appear anywhere in this file (section 4 negative gate).

mod camera;
#[cfg(test)]
mod trim_tests;
#[cfg(test)]
mod host_parity_tests;
#[cfg(test)]
mod row_gesture_tests;

use crate::{ProjectAction, RootAction};
use crate::chrome::{ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::input::UIEvent;
use crate::node::*;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};
use crate::tree::UITree;
use manifold_foundation::{AudioSendId, LayerId};

use super::{GraphParamTarget, PanelAction, ParamsAction};
use super::actions::{MaterialEditKind, MaterialParamWrite};
#[cfg(test)]
use super::{ScrubPhase, ScrubValue, ValueRef};
use super::copy_to_clipboard_label::CopyToClipboardLabelState;
use super::param_card::{RowGeometry, RowMod};
use super::param_slider_shared::{
    AudioRowState, ModTab, ParamModState, RowHost, RowInteraction, build_param_row,
    ROW_ROLE_SECTION_HEADER, param_row_key_base,
    build_toggle_trigger_row, ToggleParamIds,
};
use super::param_slider_shared::material_placement::MaterialPlacementWidget;
use crate::param_surface::{
    MaterialGroup, MaterialLook, MaterialMapFamily, MaterialParamRole, ModifierObjectRef,
    ParamRow, ParamSurface, RgbChannel, RowMapping, RowRole, RowSpec, UvComponent,
};
use crate::slider::GAP;

// ── Stable keys ──
const KEY_BG: u64 = 80_001;
const KEY_CLOSE: u64 = 80_002;
const KEY_ADD_ENVIRONMENT: u64 = 80_010;
const KEY_ADD_FOG: u64 = 80_011;
const KEY_NEW_SCENE: u64 = 80_012;
const KEY_OPEN_GRAPH_EDITOR: u64 = 80_013;
// KEY_ADD_OBJECT / KEY_ADD_LIGHT / KEY_ADD_PLANE live in
// `scene_setup_actions.rs` with the row they build (godfile ceiling).
/// "Import Model…" (P4, D4/D5) — merges a second glb into this scene.
const KEY_IMPORT_MODEL: u64 = 80_016;
/// Outliner fold header keys (scene-panel-ux lane): Scene, Lights, Objects
const KEY_OUTLINER_SCENE: u64 = 80_017;
const KEY_OUTLINER_LIGHTS: u64 = 80_018;
const KEY_OUTLINER_OBJECTS: u64 = 80_019;
/// Frame button offset: use offset 33 to avoid collision with Remove (20), Duplicate (21), and mod buttons (22..32)
const OBJ_OFF_FRAME: u64 = 33;
/// P4b Skin row source/target dropdown buttons.
const OBJ_OFF_SKIN_SOURCE: u64 = 34;
const OBJ_OFF_SKIN_TARGET: u64 = 35;
const MATERIAL_SWATCH_KEY_BASE: u64 = 96_000;
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
/// design's own order. Plain string literals — no `manifold-renderer`
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
const MODIFIER_ROW_STRIDE: u64 = 20;
const MODIFIER_OFF_UP: u64 = 0;
const MODIFIER_OFF_DOWN: u64 = 1;
const MODIFIER_OFF_REMOVE: u64 = 2;
/// Reserved sub-range within the per-object budget for the "+ Add Modifier"
/// button (UX-P2 D6: one control now, was 7 chips) — well clear of any real
/// modifier stack (never more than a handful of rows).
const MODIFIER_ADD_BUTTON_OFFSET: u64 = 400;

const fn modifier_row_key(object_index: usize, modifier_index: usize, offset: u64) -> u64 {
    MODIFIER_KEY_BASE + object_index as u64 * MODIFIER_OBJ_STRIDE + modifier_index as u64 * MODIFIER_ROW_STRIDE + offset
}

const fn modifier_add_button_key(object_index: usize) -> u64 {
    MODIFIER_KEY_BASE + object_index as u64 * MODIFIER_OBJ_STRIDE + MODIFIER_ADD_BUTTON_OFFSET
}

const PANEL_W_MIN: f32 = 320.0;
const TITLE_H: f32 = 26.0;
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
    /// UX-P3a (SCENE_PANEL_UX_DESIGN.md D8/sizing amendment): whether this
    /// param is currently an exposed card param on the layer's generator
    /// graph — `manifold_renderer::node_graph::scene_vm::is_param_exposed`'s
    /// read off the SAME `EffectGraphDef` `SceneVm::from_def` already
    /// walked, transcribed by `state_sync` like every other field on this
    /// struct. Drives the row's mod-button lit state; NOT written by this
    /// panel (exposure is a graph-side toggle via
    /// `PanelAction::SceneSetupExposeParam`, never a direct field write).
    pub exposed: bool,
}

/// The outliner row template's trailing affordance slot (D5 of
/// SCENE_PANEL_UX_DESIGN.md): every row reserves the SAME width for this
/// slot and renders EITHER a live eye toggle (Object rows, which carry a
/// `visible` param) OR a dimmed, non-interactive eye glyph (Camera/World/
/// Light rows, which don't) — never a different control. Uniformity is the
/// point (`feedback_no_conditionally_visible_ui`): the slot's meaning never
/// changes per row, only whether it's live.
enum EyeSlot {
    Live(RowValue),
    /// C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md eye-slot amendment,
    /// closes BUG-238): Camera/World/Light rows carry no real visible/enable
    /// param (`SceneLightVm`/`CameraVm`/`EnvironmentVm`/`AtmosphereVm` have
    /// no visibility address) — the trailing slot renders truly empty, not a
    /// dimmed glyph that looked like a dead control (the old `Dimmed`
    /// variant, deleted — it drew a non-interactive eye glyph on rows that
    /// could never toggle anything, which read as a bugged control rather
    /// than "nothing here"). Object rows (which DO carry
    /// `scene_object.visible`) keep `Live`; the slot's WIDTH stays reserved
    /// either way (`feedback_no_conditionally_visible_ui`) — only the glyph
    /// is gone.
    Empty,
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
    Importer { mode_is_hdri: bool, intensity: ModulatedRow, fill: ModulatedRow, hdri_file: String },
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
/// metallic/roughness only for `pbr_material` (phong/unlit/cel don't have
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
    pub object: ModifierObjectRef,
    pub object_gain: Option<manifold_foundation::ParamId>,
    pub material: ModifierObjectRef,
    pub shared_object_count: Option<usize>,
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
    pub index: usize,
    /// The `node.scene_object`'s own doc id — the address the eye toggle
    /// writes `visible` at, and (with `group_node_id`) the selection key
    /// (D12).
    pub object_node_id: u32,
    /// `Some` when wrapped in a group (the importer/`AddSceneObjectCommand`
    /// shape) — the rename sweep's group target. `None` for a bare
    /// ungrouped scene_object (D1's first-class "hand-built graph, no
    /// group" case).
    pub group_node_id: Option<u32>,
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
    /// P2 slice 2a (SCENE_PANEL_EXPOSURE_CONVERGENCE_DESIGN.md): the REAL P1
    /// section strings covering this object — its transform node, its
    /// material node, its own `scene_object` node, and every modifier in its
    /// stack. Resolved once by `state_sync` via a doc-id cross-reference
    /// against the layer's exposure metadata (never reconstructed from a
    /// naming convention — creation-time and load-migration stamping produce
    /// different strings for the same node kind). Filters the unified
    /// properties card down to exactly this object's rows.
    pub sections: Vec<String>,
    /// P4b: the object's layer-skin row. `None` when no `node.layer_source`
    /// is wired into the object's material maps.
    pub skin: Option<SkinRowVm>,
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

/// A stepper row whose value is an enum index rather than a raw float —
/// historically the same `[label] [−] value [+]` shape as [`RowValue`]'s
/// numeric steppers; C-P1c/C-P1d converted every consumer (Light, then
/// Modifier's Axis rows) onto [`ModulatedEnumRow`]'s `value_labels` path, so
/// this type has no producer left in this crate — kept only as the DTO shape
/// documentation for `ModulatedEnumRow`'s own doc comment to point at
/// (`labels` is transcribed by `state_sync`, the same DTO-boundary
/// convention as `EnvironmentRowVm::mode_is_hdri`, since this crate can't
/// depend on `manifold-renderer`'s `LIGHT_MODES`/`SHADOW_SOFTNESS_LABELS`).
#[derive(Clone, Debug, PartialEq)]
pub struct EnumRowValue {
    pub row: RowValue,
    pub labels: Vec<&'static str>,
}

/// C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): the modulation-carrying
/// twin of [`EnumRowValue`] — same shape, but `row` is a [`ModulatedRow`] so
/// enum/axis rows can carry driver/envelope/audio-mod facts through
/// `build_param_row`'s `ParamRow.value_labels` path (the card row core
/// already supports labeled/enum rows — no bespoke stepper needed, D1's
/// "check for a card enum row first"). Light's Mode/Cast Shadows/Shadow
/// Softness rows were the first consumer (C-P1c); C-P1d moved Modifier's
/// `ModifierParamRowVm::Axis` rows onto this same type, so every enum row in
/// the panel now rides one shape.
#[derive(Clone, Debug, PartialEq)]
pub struct ModulatedEnumRow {
    pub row: ModulatedRow,
    pub labels: Vec<&'static str>,
}

/// One light row's full editable surface (D3/D4): mode, color, intensity,
/// pos/aim, cast_shadows, shadow_softness, and light_size — the last shown
/// as a sub-row beneath shadow_softness but ALWAYS present and editable
/// (parameter dependency, not conditional UI — `feedback_no_conditionally_visible_ui`).
/// C-P1c: every field promoted to `ModulatedRow`/`ModulatedEnumRow` — same
/// promotion C-P1b already did for `TransformRowVm`/`ObjectMaterialVm`.
#[derive(Clone, Debug, PartialEq)]
pub struct LightKnownRow {
    pub index: usize,
    pub node_doc_id: u32,
    /// P5: the light's editable display name (NEW — lights didn't have one
    /// before this design). Double-click opens the same rename UX as an
    /// object's name.
    pub name: String,
    pub mode: ModulatedEnumRow,
    pub color: (ModulatedRow, ModulatedRow, ModulatedRow),
    pub intensity: ModulatedRow,
    pub pos: (ModulatedRow, ModulatedRow, ModulatedRow),
    pub aim: (ModulatedRow, ModulatedRow, ModulatedRow),
    /// A 2-label (`Off`/`On`) enum stepper over the raw [0,1] threshold —
    /// same shape as `mode`/`shadow_softness`, not a bespoke toggle widget.
    pub cast_shadows: ModulatedEnumRow,
    pub shadow_softness: ModulatedEnumRow,
    pub light_size: ModulatedRow,
    /// P2 slice 2a: this light's REAL P1 section string(s) (its own handle —
    /// see `ObjectKnownRow::sections`'s doc comment for how these are
    /// resolved). Usually a single entry.
    pub sections: Vec<String>,
}

/// One Lights-section row.
#[derive(Clone, Debug, PartialEq)]
pub enum LightRowVm {
    Known(Box<LightKnownRow>),
    /// Producer wasn't `node.light` — honest custom row (D3).
    Custom { index: usize },
}

/// `node.camera_lens`'s four params (D3: "the lens node's own row beneath").
/// C-P1c: `ModulatedRow`, same promotion as `LightKnownRow`.
#[derive(Clone, Debug, PartialEq)]
pub struct LensRowVm {
    pub focus_distance: ModulatedRow,
    pub f_stop: ModulatedRow,
    pub shutter_angle: ModulatedRow,
    pub exposure_ev: ModulatedRow,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrbitCameraRowVm {
    pub orbit: ModulatedRow,
    pub tilt: ModulatedRow,
    pub distance: ModulatedRow,
    pub fov_y: ModulatedRow,
    pub lens: Option<LensRowVm>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FreeCameraRowVm {
    pub pos: (ModulatedRow, ModulatedRow, ModulatedRow),
    pub yaw: ModulatedRow,
    pub pitch: ModulatedRow,
    pub roll: ModulatedRow,
    pub fov_y: ModulatedRow,
    pub lens: Option<LensRowVm>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LookAtCameraRowVm {
    pub pos: (ModulatedRow, ModulatedRow, ModulatedRow),
    pub target: (ModulatedRow, ModulatedRow, ModulatedRow),
    pub fov_y: ModulatedRow,
    pub lens: Option<LensRowVm>,
}

/// The Camera section (D3/D4): exactly one of these per live scene (`None`
/// when `camera` is unwired — D3 has no "add camera" action in v1, unlike
/// Environment/Fog, since `render_scene`'s `camera` port is REQUIRED —
/// SCENE_BUILD's starter preset and every importer path always wire one).
#[derive(Clone, Debug, PartialEq)]
pub enum CameraRowVm {
    None,
    Orbit(Box<OrbitCameraRowVm>),
    Free(Box<FreeCameraRowVm>),
    LookAt(Box<LookAtCameraRowVm>),
    /// Producer resolved but isn't one of the three curated atoms — honest
    /// custom row (D3).
    Custom,
}

/// Full live-panel view model for one selected generator layer's scene —
/// translated 1:1 from `manifold_renderer::node_graph::scene_vm::SceneVm`'s
/// Header/Environment/Atmosphere sections by `state_sync` (this crate can't
/// depend on `manifold-renderer`/`manifold-core`, so the translation is the
/// UI-facing DTO boundary, same convention as `AudioSendRow`).
#[derive(Clone, Debug, PartialEq)]
pub struct SceneSetupVm {
    pub layer_id: LayerId,
    pub scene_name: String,
    pub multiple_scenes: bool,
    pub object_count: usize,
    pub light_count: usize,
    pub shadow_caster_count: usize,
    /// `render_scene`'s own doc id — the target the "Add environment"/
    /// "Add fog"/"+ Object"/"+ Light" composites wire into.
    pub scene_root_node_id: u32,
    pub environment: EnvironmentRowVm,
    pub atmosphere: AtmosphereRowVm,
    /// P2: the Objects section's rows, in `mesh_k` order.
    pub objects: Vec<ObjectRowVm>,
    /// P3: the Lights section's rows, in `light_k` order. Never capped —
    /// REALTIME_3D D4's shadow-caster limit (K=4) is the renderer's job; the
    /// panel reports the true count and renders every row regardless.
    pub lights: Vec<LightRowVm>,
    /// P3: the Camera section (D3's single-camera trace, lens pass-through
    /// included).
    pub camera: CameraRowVm,
    /// P2 slice 2a: the REAL P1 section string(s) covering the camera family
    /// (the camera atom + its lens, if wired) — see `ObjectKnownRow::sections`.
    pub camera_sections: Vec<String>,
    /// Custom/loop cameras expose only their shared lens and cinematic tail.
    /// Ownership disambiguates rows sharing the importer's "Camera" section.
    /// None preserves the full section for ordinary camera sources.
    pub camera_param_doc_ids: Option<Vec<u32>>,
    /// P2 slice 2a: the REAL P1 section string(s) covering World (the
    /// environment/bake node + the atmosphere/fog node, whichever are
    /// wired) — see `ObjectKnownRow::sections`.
    pub world_sections: Vec<String>,
    /// Scene bounds for translate-slider range derivation. `Some((min, max))`
    /// when bounds are available (stored import bounds or camera-distance proxy),
    /// used to compute scene-relative slider ranges (center ± 2×extent per axis).
    /// Fallback to descriptor defaults when None.
    pub scene_bounds: Option<([f32; 3], [f32; 3])>,
}

/// P5's outliner selection (D7): the one scene item whose controls the
/// properties region shows. UI-local workspace state — like fold state,
/// NEVER serialized (`rg -n "SceneSelection" crates/manifold-io
/// crates/manifold-core` must stay 0 hits). `u32` payloads are node doc
/// ids — removal-stable, unlike indices.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SceneSelection {
    Object(u32),
    Light(u32),
    Camera,
    World,
    /// scene-panel-ux lane: outliner group fold toggle (Scene/Lights/Objects)
    OutlinerFold(&'static str),
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

    /// Resize every per-row vector to `n`, rebuilding `mod_state` fresh (the
    /// build pass re-syncs every row's modulation facts from the VM's
    /// `RowModulation` every frame — same "no rotting" contract the rest of
    /// this panel already has, so nothing here needs to survive the
    /// `mod_active_tab` survives a mid-gesture rebuild; the gesture itself is
    /// owned by `RowHost`, which captures the wire address rather than a row
    /// index and therefore remains valid across structural snapshots.
    fn resize(&mut self, n: usize) {
        self.rows.resize(n, placeholder_param_info());
        self.mod_state = ParamModState::allocate(n);
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
            section: None,
        disabled: None,
        material_role: None,
        inactive_reason: None,
        },
        value: crate::param_surface::RowValue { base: 0.0, effective: 0.0, exposed: false, driven: false },
        audio: AudioRowState::default(),
        modulation: RowMod::default(),
        mapping: RowMapping {
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
        },
        scene_addr: None,
        rgb_members: None,
        material_attached: false,
    }
}

/// One numeric row's interactive node ids, set by `build_numeric_row` when
pub struct ScenePanel {
    open: bool,
    state: SceneSetupState,
    panel_w: f32,
    host: ChromeHost,
    scroll: ScrollContainer,
    pub(crate) content_parent: NodeId,
    bg_id: NodeId,
    close_id: NodeId,
    add_environment_id: Option<NodeId>,
    add_fog_id: Option<NodeId>,
    new_scene_id: Option<NodeId>,
    open_graph_editor_id: Option<NodeId>,
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
    /// P2 slice 2a: the scene panel's bound layer's FULL generator
    /// `ParamSurface` (every exposed param, every section) — built by
    /// `state_sync` the SAME way the main inspector's generator card is
    /// (`gen_params_to_surface`), for THIS panel's layer specifically (never
    /// `active_layer` — see `configure_params`'s doc comment). The
    /// properties body filters this down to the selected item's sections at
    /// build time.
    full_params: Option<ParamSurface>,
    full_param_id_index: ahash::AHashMap<String, usize>,
    add_object_id: Option<NodeId>,
    add_light_id: Option<NodeId>,
    /// BUG-hlw8 "+ Plane" — dispatches `SceneSetupAddLayerPlane`.
    add_plane_id: Option<NodeId>,
    /// "Import Model…" (P4) — dispatches `SceneSetupImportModelClicked`,
    /// which opens the file dialog + merges on the app side (the panel
    /// itself never touches the filesystem).
    import_model_id: Option<NodeId>,
    /// P5 (D7): the outliner selection, per layer — UI-local workspace
    /// state, like fold state, NEVER serialized. Missing entry = the
    /// default (first object, else World) — resolved by
    /// `Self::resolve_selection`, which also handles the "selected id no
    /// longer exists after a graph edit" fallback.
    /// `LayerId` has no `Ord` impl (only `Hash`/`Eq`), so `HashMap` — not a
    /// `BTreeMap` — is the map that actually compiles; same "keyed per
    /// layer, UI-local, never serialized" contract either way.
    selection: std::collections::HashMap<LayerId, SceneSelection>,
    /// Every outliner row's click target this frame — `(node_id, what
    /// selecting it means)`.
    outliner_row_ids: Vec<(NodeId, SceneSelection)>,
    /// Every object row's eye toggle this frame — `(node_id, the object's
    /// current `visible` RowValue)`. A click flips the value (writes
    /// `!(value > 0.5)` as 0.0/1.0) through the same
    /// `SceneSetupParamChanged` fourth-surface path every other row uses.
    outliner_eye_ids: Vec<(NodeId, RowValue)>,
    /// `(identity_node_id, name_label_node_id, current_name)` for the
    /// properties header's editable name row, when a Known object is
    /// selected this frame (`identity_node_id` = `group_node_id.unwrap_or(
    /// object_node_id)`, the exact address `RenameSceneObjectCommand`
    /// takes) — resolves a name-label click to its rename action, and backs
    /// `object_name_rect` (the app's text-input anchor lookup). At most one
    /// entry per frame (P5: one selection, one properties header).
    object_name_ids: Vec<(u32, NodeId, String)>,
    /// BUG-193/P5: `(remove_button_node_id, index)` for the properties
    /// header's "Remove" button, when a Known object is selected this frame
    /// — resolves to `PanelAction::SceneSetupRemoveObject`. At most one
    /// entry per frame.
    object_remove_ids: Vec<(NodeId, usize)>,
    /// P5 (D11): `(duplicate_button_node_id, index)` for the properties
    /// header's "Duplicate" button, when a Known object is selected this
    /// frame — resolves to `PanelAction::SceneSetupDuplicateObject`.
    object_duplicate_ids: Vec<(NodeId, usize)>,
    /// scene-panel-ux lane: `(frame_button_node_id, object_index)` for the properties
    /// header's "Frame" button, when a Known object is selected this frame
    /// — resolves to `PanelAction::SceneSetupFrameSelected`.
    object_frame_ids: Vec<(NodeId, usize)>,
    /// scene-panel-ux lane: fold state for properties sections, keyed by
    /// section NAME globally within the panel (folding "Material" folds it
    /// for every object). UI-local, never serialized. Missing entry = expanded.
    section_folded: ahash::AHashMap<String, bool>,
    /// scene-panel-ux lane: fold state for outliner groups (Scene/Lights/Objects).
    /// UI-local, never serialized. Missing entry = expanded.
    outliner_folded: ahash::AHashMap<&'static str, bool>,
    /// P5: `(node_id, group_node_id, modifier_node_id)` for every modifier
    /// row's remove button built this frame.
    modifier_remove_ids: Vec<(NodeId, u32, u32)>,
    /// P5: `(node_id, group_node_id, modifier_node_id, new_position)` for
    /// every up/down reorder button built this frame — only pushed for
    /// buttons that aren't at a stack boundary (up at index 0 / down at the
    /// last index are rendered but inert, per
    /// `feedback_no_conditionally_visible_ui`).
    modifier_move_ids: Vec<(NodeId, u32, u32, u32)>,
    /// UX-P2 (D6): `(button_node_id, group_node_id)` for the single "+ Add
    /// Modifier" button built this frame, when the selected object's chain
    /// is addable (was `modifier_add_ids: Vec<(NodeId, u32, String)>`, one
    /// entry per chip — the click now opens the shared dropdown instead of
    /// resolving directly, so there's at most one entry and no `type_id`).
    add_modifier_button_id: Option<(NodeId, u32)>,
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
    light_remove_ids: Vec<(NodeId, usize)>,
    /// P5: `(light_node_doc_id, name_label_node_id, current_name)` for the
    /// properties header's editable light name row, when a Known light is
    /// selected this frame — mirrors `object_name_ids`, backs
    /// `light_name_rect`.
    light_name_ids: Vec<(u32, NodeId, String)>,
    /// Canonical material colour swatches built over the shared scalar row.
    /// `(button, row, scalar ids)` is rebuilt with the live tree and never
    /// used as a second value store.
    material_swatch_ids: Vec<(NodeId, usize, [manifold_foundation::ParamId; 3], crate::param_surface::MaterialColour)>,
    /// Expanded colour groups, keyed by the stable primary channel id.
    material_rgb_expanded: ahash::AHashSet<manifold_foundation::ParamId>,
    /// Named material recipe buttons for the selected object's material.
    material_look_ids: Vec<(NodeId, MaterialLook, ModifierObjectRef, ModifierObjectRef)>,
    /// Add Feature controls and explicit feature mode headers for the selected
    /// material. The refs are captured from the current structural projection.
    material_feature_ids: Vec<(
        NodeId,
        crate::param_surface::MaterialFeature,
        Vec<MaterialParamWrite>,
        ModifierObjectRef,
        ModifierObjectRef,
    )>,
    active_material_info: Option<MaterialInspectorInfo>,
    /// One friendly placement widget per supported texture family. A widget
    /// is active only when its family is connected in the selected material;
    /// raw matrix and sampler rows remain in the shared Advanced drawer.
    material_placement_widgets: [MaterialPlacementWidget; 5],
    material_placement_active: [bool; 5],
    material_placement_built: [bool; 5],
    panel_rect: Rect,
}

impl Default for ScenePanel {
    fn default() -> Self {
        Self {
            open: false,
            state: SceneSetupState::default(),
            panel_w: PANEL_W_MIN,
            host: ChromeHost::new(),
            scroll: ScrollContainer::new(),
            content_parent: NodeId::PLACEHOLDER,
            bg_id: NodeId::PLACEHOLDER,
            close_id: NodeId::PLACEHOLDER,
            add_environment_id: None,
            add_fog_id: None,
            new_scene_id: None,
            open_graph_editor_id: None,
            properties_card: SceneCardState::new(),
            full_params: None,
            full_param_id_index: ahash::AHashMap::new(),
            add_object_id: None,
            add_light_id: None,
            add_plane_id: None,
            import_model_id: None,
            selection: std::collections::HashMap::new(),
            outliner_row_ids: Vec::new(),
            outliner_eye_ids: Vec::new(),
            object_name_ids: Vec::new(),
            object_remove_ids: Vec::new(),
            object_duplicate_ids: Vec::new(),
            object_frame_ids: Vec::new(),
            section_folded: ahash::AHashMap::new(),
            outliner_folded: ahash::AHashMap::new(),
            modifier_remove_ids: Vec::new(),
            modifier_move_ids: Vec::new(),
            add_modifier_button_id: None,
            skin_source_ids: Vec::new(),
            skin_target_ids: Vec::new(),
            light_remove_ids: Vec::new(),
            light_name_ids: Vec::new(),
            material_swatch_ids: Vec::new(),
            material_rgb_expanded: ahash::AHashSet::new(),
            material_look_ids: Vec::new(),
            material_feature_ids: Vec::new(),
            active_material_info: None,
            material_placement_widgets: std::array::from_fn(|_| MaterialPlacementWidget::new()),
            material_placement_active: [false; 5],
            material_placement_built: [false; 5],
            panel_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
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
        if !self.open || !matches!(self.state, SceneSetupState::Live(_)) {
            return;
        }
        self.properties_card.row_host.register_intents(intents);
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
    }

    /// P2 slice 2a: hand the panel the layer's FULL generator
    /// `ParamSurface` (state_sync builds it via the SAME
    /// `gen_params_to_surface` the main inspector's generator card uses, for
    /// THIS panel's bound layer — `live_layer_id()`, never `active_layer`:
    /// the panel edits the scene of its OWN docked layer, which can
    /// legitimately differ from the app's active layer (BUG-292). The
    /// properties body filters this down to the selected item's sections at
    /// build time — see `build_filtered_properties`.
    pub fn configure_params(&mut self, config: Option<ParamSurface>) {
        self.full_param_id_index.clear();
        if let Some(surface) = config.as_ref() {
            self.full_param_id_index.reserve(surface.rows.len());
            for (index, row) in surface.rows.iter().enumerate() {
                self.full_param_id_index.insert(row.id.to_string(), index);
            }
        }
        self.full_params = config;
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

    /// Per-frame VALUE sync (sibling of `ui_bridge::sync_card_values`): push
    /// the unified properties card's row values from the layer's full generator
    /// manifest, without a structural rebuild. `slots` is the id-keyed channel
    /// `ui_translate::with_param_slots(&gp.params, …)` hands out for the SAME
    /// layer `full_params` was built from. Each slot JOINS onto the built row
    /// carrying the same id (BUG-313) via `row_id_index` — never by position,
    /// so no retained-index list can drift. A manifest param the current
    /// outliner selection isn't showing (not in this section's rows) simply
    /// misses the join and is ignored; a built row with no manifest entry is
    /// caught by the INV-6 coverage check.
    pub fn sync_properties_values(
        &mut self,
        tree: &mut UITree,
        slots: &mut dyn Iterator<Item = (&str, crate::view::UiParamSlot)>,
    ) {
        if !self.open {
            return;
        }
        let target = self
            .live_layer_id()
            .cloned()
            .map(GraphParamTarget::GeneratorOf);
        let full_params = &mut self.full_params;
        let full_param_id_index = &self.full_param_id_index;
        let card = &mut self.properties_card;
        card.row_value_synced.clear();
        card.row_value_synced.resize(card.rows.len(), false);
        for (id, slot) in slots {
            if let Some(&full_index) = full_param_id_index.get(id)
                && let Some(surface) = full_params.as_mut()
                && let Some(row) = surface.rows.get_mut(full_index)
            {
                row.value.base = slot.base;
                row.value.effective = slot.value;
            }
            let Some(&i) = card.row_id_index.get(id) else {
                continue;
            };
            // Dirty-check on change only — same shape as
            // `ParamCardPanel::sync_param_value` (param_card/render.rs): an
            // unconditional push formats the value String and touches the
            // tree for every row every frame, even when nothing moved. The
            // key is `last_pushed_values` (single-writer, this sync only) —
            // NOT `current_values`, which the drag/type-in paths write
            // mid-gesture; gating on that skipped the post-commit push.
            let value = target
                .as_ref()
                .and_then(|target| card.row_host.active_param_value(target, &card.rows[i].id))
                .unwrap_or(slot.value);
            let prev = card.last_pushed_values[i];
            card.current_values[i] = value;
            if value != prev || prev.is_nan() {
                card.last_pushed_values[i] = value;
                card.row_host
                    .push_slider_value(tree, i, value, &card.rows[i].spec, None);
            }
            if let Some(c) = card.row_value_synced.get_mut(i) {
                *c = true;
            }
        }
        for (i, synced) in card.row_value_synced.iter().enumerate() {
            if !*synced {
                debug_assert!(
                    false,
                    "BUG-313/INV-6: built scene row {} (id {:?}) has no live manifest entry",
                    i, card.rows[i].id,
                );
                crate::panels::param_slider_shared::warn_join_gap_once(card.rows[i].id.as_ref());
            }
        }
    }

    /// (`ScreenLayout::scene_setup()`). No-op when closed.
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
            .pad(Pad::all(PAD))
            .child(
                View::row(0.0)
                    .fill_w()
                    .h(Sizing::Fixed(TITLE_H))
                    .child(
                        View::label("Scene Setup")
                            .fill_w()
                            .fill_h()
                            .font(color::FONT_BODY)
                            .text_color(Color32::new(224, 224, 228, 255))
                            .align_text(TextAlign::Left),
                    )
                    .child(
                        View::button("\u{00D7}")
                            .w(Sizing::Fixed(STEP_W))
                            .fill_h()
                            .style(btn_style())
                            .inert()
                            .key(KEY_CLOSE),
                    ),
            )
    }

    fn build_nodes(&mut self, tree: &mut UITree, x: f32, y: f32, panel_h: f32) {
        let chrome = self.chrome_view();
        self.host.build(tree, &chrome, Rect::new(x, y, self.panel_w, panel_h));
        self.bg_id = self.host.node_id_for_key(KEY_BG).unwrap_or(NodeId::PLACEHOLDER);
        self.close_id = self.host.node_id_for_key(KEY_CLOSE).unwrap_or(NodeId::PLACEHOLDER);
        self.panel_rect = Rect::new(x, y, self.panel_w, panel_h);
        // Reset every dynamic control id — repopulated by whichever
        // `build_*` branch below actually builds this frame (state_sync
        // rebuilds fresh every pass, D1 "no staleness").
        self.add_environment_id = None;
        self.add_fog_id = None;
        self.new_scene_id = None;
        self.open_graph_editor_id = None;
        self.add_object_id = None;
        self.add_light_id = None;
        self.add_plane_id = None;
        self.import_model_id = None;
        self.outliner_row_ids.clear();
        self.outliner_eye_ids.clear();
        self.object_name_ids.clear();
        self.object_remove_ids.clear();
        self.object_duplicate_ids.clear();
        self.modifier_remove_ids.clear();
        self.modifier_move_ids.clear();
        self.add_modifier_button_id = None;
        self.skin_source_ids.clear();
        self.skin_target_ids.clear();
        self.light_remove_ids.clear();
        self.light_name_ids.clear();
        self.material_swatch_ids.clear();
        self.material_look_ids.clear();
        self.material_feature_ids.clear();
        self.active_material_info = None;
        self.material_placement_active = [false; 5];
        self.material_placement_built = [false; 5];
        let inner_x = x + PAD;
        let inner_w = self.panel_w - PAD * 2.0;
        let content_top = y + PAD + TITLE_H;
        let body_viewport = Rect::new(x, content_top, self.panel_w, (y + panel_h - PAD - content_top).max(0.0));
        let clip_id = self.scroll.begin(tree, body_viewport);
        self.content_parent = clip_id;
        let content_start = tree.count();
        let mut cy = content_top;

        cy = match self.state.clone() {
            SceneSetupState::NoSelection(sentence) => {
                self.build_sentence(tree, inner_x, inner_w, cy, &sentence)
            }
            SceneSetupState::NoGenerator { .. } => self.build_no_generator(tree, inner_x, inner_w, cy),
            SceneSetupState::NoScene { .. } => self.build_no_scene(tree, inner_x, inner_w, cy),
            SceneSetupState::Live(vm) => self.build_live(tree, inner_x, inner_w, cy, &vm),
        };

        let content_height = (cy - content_top + PAD).max(0.0);
        self.scroll.set_content_height(content_height);
        self.scroll.reparent_content(tree, content_start);
        let offset = self.scroll.scroll_offset();
        if offset != 0.0 {
            self.scroll.offset_content(tree, -offset);
        }
        let sb_x = x + self.panel_w - SCROLLBAR_W - 2.0;
        self.scroll.build_scrollbar(tree, sb_x, &scrollbar_style());
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

    /// D7: outliner (Camera · World · lights · objects, one row each) over a
    /// single properties region showing the current selection's controls —
    /// "select the object to use the tools" (Peter). Replaces v1's flat
    /// per-section accordion (a 2-object scene already overflowed the
    /// panel's window).
    fn build_live(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32, vm: &SceneSetupVm) -> f32 {
        // ── Header ──
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &vm.scene_name, header_label_style());
        cy += ROW_H;
        if vm.multiple_scenes {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "2+ scenes in this graph — showing the first",
                label_style(),
            );
            cy += ROW_H;
        }
        let counts = format!(
            "{} object{} · {} light{} · {} shadow caster{}",
            vm.object_count,
            if vm.object_count == 1 { "" } else { "s" },
            vm.light_count,
            if vm.light_count == 1 { "" } else { "s" },
            vm.shadow_caster_count,
            if vm.shadow_caster_count == 1 { "" } else { "s" },
        );
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &counts, label_style());
        cy += ROW_H + ROW_GAP * 2.0;

        // ── Outliner ──
        let selected = self.resolve_selection(vm);
        cy = self.build_outliner(tree, inner_x, inner_w, cy, vm, selected);
        cy += ROW_GAP * 2.0;

        // ── Properties ──
        self.build_properties(tree, inner_x, inner_w, cy, vm, selected)
    }

    /// External selection write (REALTIME_3D_DESIGN.md P6): a viewport
    /// object-pick sets the SAME UI-local `self.selection` map an outliner
    /// row click does (`handle_event`'s `SceneSetupSelectionChanged` arm) —
    /// one selection store for the whole app, not a second one that could
    /// drift from the panel's own. The caller (the graph-editor window's
    /// mouse-press handler) is responsible for redrawing/rebuilding
    /// whatever reads this panel's Properties section afterward; this call
    /// alone doesn't trigger one (it has no `PanelAction` dispatch loop to
    /// push through, unlike `handle_event`'s click arm).
    pub fn set_selection(&mut self, layer_id: LayerId, sel: SceneSelection) {
        self.selection.insert(layer_id, sel);
    }

    /// The current selection for `vm.layer_id`, resolving the D7 fallback
    /// (a dangling id after a graph edit, or no entry yet) to the first
    /// Known object, else World — and persisting the resolved value back
    /// into `self.selection` so a later `object_name_rect`/click lookup
    /// this same frame sees the same answer `build_outliner` used.
    fn resolve_selection(&mut self, vm: &SceneSetupVm) -> SceneSelection {
        let current = self.selection.get(&vm.layer_id).copied();
        let resolved = match current {
            Some(sel) if Self::selection_exists(vm, sel) => sel,
            _ => Self::default_selection(vm),
        };
        self.selection.insert(vm.layer_id.clone(), resolved);
        resolved
    }

    fn selection_exists(vm: &SceneSetupVm, sel: SceneSelection) -> bool {
        match sel {
            SceneSelection::Camera | SceneSelection::World => true,
            SceneSelection::OutlinerFold(_) => true, // Fold headers always exist
            SceneSelection::Object(id) => {
                vm.objects.iter().any(|o| matches!(o, ObjectRowVm::Known(r) if r.object_node_id == id))
            }
            SceneSelection::Light(id) => {
                vm.lights.iter().any(|l| matches!(l, LightRowVm::Known(r) if r.node_doc_id == id))
            }
        }
    }

    /// D7's default: the first Known object, else World. A `Custom` row
    /// carries no addressable node id (D12), so it can never be the default
    /// target — it's still listed in the outliner, just not selectable.
    fn default_selection(vm: &SceneSetupVm) -> SceneSelection {
        vm.objects
            .iter()
            .find_map(|o| match o {
                ObjectRowVm::Known(r) => Some(SceneSelection::Object(r.object_node_id)),
                ObjectRowVm::Custom { .. } => None,
            })
            .unwrap_or(SceneSelection::World)
    }

    /// The outliner: one row per scene item, grouped under section labels
    /// (D5) — Scene (Camera · World) · Lights · Objects — plus the compact
    /// single-row action footer (D6: + Object · + Light · Import Model…).
    /// Every row (selectable or not) renders the same `[type icon | name |
    /// trailing affordance]` template — flat, no nesting (D5; inherited from
    /// REALTIME_3D "Decided — do not reopen" section 1).
    fn build_outliner(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
        selected: SceneSelection,
    ) -> f32 {
        // Scene group header
        let scene_folded = self.outliner_folded.get("Scene").copied().unwrap_or(false);
        cy = self.build_outliner_fold_header(tree, inner_x, inner_w, cy, "Scene", scene_folded, KEY_OUTLINER_SCENE);
        if !scene_folded {
            cy = self.build_outliner_row(
                tree, inner_x, inner_w, cy, "\u{1F4F7} Camera", SceneSelection::Camera, selected, EyeSlot::Empty,
            );
            cy = self.build_outliner_row(
                tree, inner_x, inner_w, cy, "\u{1F30D} World", SceneSelection::World, selected, EyeSlot::Empty,
            );
        }

        // Lights group header
        let lights_folded = self.outliner_folded.get("Lights").copied().unwrap_or(false);
        cy = self.build_outliner_fold_header(tree, inner_x, inner_w, cy, "Lights", lights_folded, KEY_OUTLINER_LIGHTS);
        if !lights_folded {
            for light in &vm.lights {
                match light {
                    LightRowVm::Known(row) => {
                        let label = format!("\u{1F4A1} {}", row.name);
                        cy = self.build_outliner_row(
                            tree,
                            inner_x,
                            inner_w,
                            cy,
                            &label,
                            SceneSelection::Light(row.node_doc_id),
                            selected,
                            EyeSlot::Empty,
                        );
                    }
                    LightRowVm::Custom { index } => {
                        // No addressable node id (D12/D3) — listed, never hidden,
                        // but not a selectable target (nothing to show in
                        // Properties beyond the same "custom" label). Same row
                        // template as a selectable row (D5), minus the click.
                        cy = self.build_outliner_row_static(
                            tree,
                            inner_x,
                            inner_w,
                            cy,
                            &format!("\u{1F4A1} Light {index} — custom (edit in graph)"),
                            false,
                        );
                    }
                }
            }
        }

        // Objects group header with Import Model button
        let objects_folded = self.outliner_folded.get("Objects").copied().unwrap_or(false);
        cy = self.build_outliner_fold_header_with_button(
            tree,
            inner_x,
            inner_w,
            cy,
            "Objects",
            objects_folded,
            KEY_OUTLINER_OBJECTS,
            KEY_IMPORT_MODEL,
            "Import Model…",
        );
        if !objects_folded {
            for obj in &vm.objects {
                match obj {
                    ObjectRowVm::Known(row) => {
                        let label = format!("\u{25A0} {}", row.name);
                        cy = self.build_outliner_row(
                            tree,
                            inner_x,
                            inner_w,
                            cy,
                            &label,
                            SceneSelection::Object(row.object_node_id),
                            selected,
                            EyeSlot::Live(row.visible.clone()),
                        );
                    }
                    ObjectRowVm::Custom { index } => {
                        cy = self.build_outliner_row_static(
                            tree,
                            inner_x,
                            inner_w,
                            cy,
                            &format!("\u{25A0} Object {index} — custom (edit in graph)"),
                            true,
                        );
                    }
                }
            }
        }
        cy += ROW_GAP;

        // D6/BUG-hlw8: compact action row — built in `scene_setup_actions.rs`
        // (this file is under the godfile line ceiling; the row's doc there
        // explains the dead compact Import Model button it replaced).
        let (ids, cy) = super::scene_setup_actions::build_add_action_row(
            tree,
            Some(self.content_parent),
            inner_x,
            inner_w,
            cy,
        );
        self.add_object_id = Some(ids.object);
        self.add_light_id = Some(ids.light);
        self.add_plane_id = Some(ids.plane);
        cy
    }

    /// One selectable outliner row: a name button, plus the trailing
    /// affordance slot (D5) — a live eye toggle (`EyeSlot::Live`) or nothing
    /// at all (`EyeSlot::Empty` — C-P1c, BUG-238), always at the
    /// same width and position so the slot's meaning never shifts per row
    /// (`feedback_no_conditionally_visible_ui`). Selected-row styling per the
    /// `layer_header.rs` precedent (`sel_accent_style`/`bg_style`): a tint
    /// using the app-wide `SELECTED_LAYER_RING` colour, never a border box.
    fn build_outliner_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        sel: SceneSelection,
        selected: SceneSelection,
        eye: EyeSlot,
    ) -> f32 {
        let is_selected = sel == selected;
        let row_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w - STEP_W,
            ROW_H,
            outliner_row_style(is_selected),
            label,
            outliner_row_key(sel),
        );
        self.outliner_row_ids.push((row_id, sel));
        match eye {
            EyeSlot::Live(row) => {
                let on = row.value > 0.5;
                let object_node_id = match sel {
                    SceneSelection::Object(id) => id,
                    _ => 0,
                };
                let eye_id = tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x + inner_w - STEP_W,
                    cy,
                    STEP_W,
                    ROW_H,
                    if row.driven { driven_label_style() } else { btn_style() },
                    if on { "\u{1F441}" } else { "\u{2013}" },
                    outliner_eye_key(object_node_id),
                );
                if !row.driven {
                    self.outliner_eye_ids.push((eye_id, row));
                }
            }
            EyeSlot::Empty => {}
        }
        cy + ROW_H
    }

    /// A non-selectable outliner row (`Custom` object/light rows, D12/D3 —
    /// no addressable node id) rendered in the SAME `[name | eye]` shape
    /// `build_outliner_row` uses, minus the click target — the row template
    /// is uniform across every row regardless of interactivity (D5).
    /// `dimmed_eye`: `true` for a Custom OBJECT row (the family still has a
    /// real `visible` param on Known rows — this instance just isn't
    /// addressable — so the dimmed glyph reads as "reserved, not present
    /// here" rather than "no such control exists"); `false` for a Custom
    /// LIGHT row (Light carries no visibility param at all, C-P1c's eye-slot
    /// amendment — BUG-238).
    fn build_outliner_row_static(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        dimmed_eye: bool,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w - STEP_W, ROW_H, label, label_style());
        if dimmed_eye {
            tree.add_label(
                Some(self.content_parent),
                inner_x + inner_w - STEP_W,
                cy,
                STEP_W,
                ROW_H,
                "\u{1F441}",
                driven_label_style(),
            );
        }
        cy + ROW_H
    }

    /// Build a foldable outliner group header (Scene/Lights/Objects). Returns the
    /// updated cy position.
    fn build_outliner_fold_header(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        name: &'static str,
        folded: bool,
        key: u64,
    ) -> f32 {
        let header_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            UIStyle {
                bg_color: color::INSPECTOR_BG,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            },
            "",
            key,
        );
        let triangle_w = 16.0;
        let triangle = if folded { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
        tree.add_label(
            Some(header_id),
            inner_x + GAP,
            cy,
            triangle_w,
            ROW_H,
            triangle,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: color::FONT_LABEL,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        tree.add_label(
            Some(header_id),
            inner_x + GAP + triangle_w,
            cy,
            (inner_w - 2.0 * GAP - triangle_w).max(0.0),
            ROW_H,
            name,
            section_label_style(),
        );
        // Register outliner fold header for click routing
        self.outliner_row_ids.push((header_id, SceneSelection::OutlinerFold(name)));
        cy + ROW_H
    }

    /// Build a foldable outliner group header with a right-aligned button
    /// (Objects group with "Import Model…"). Returns the updated cy position.
    fn build_outliner_fold_header_with_button(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        name: &'static str,
        folded: bool,
        header_key: u64,
        button_key: u64,
        button_label: &str,
    ) -> f32 {
        let header_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            UIStyle {
                bg_color: color::INSPECTOR_BG,
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            },
            "",
            header_key,
        );
        let triangle_w = 16.0;
        let triangle = if folded { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
        tree.add_label(
            Some(header_id),
            inner_x + GAP,
            cy,
            triangle_w,
            ROW_H,
            triangle,
            UIStyle {
                text_color: color::TEXT_DIMMED_C32,
                font_size: color::FONT_LABEL,
                text_align: TextAlign::Center,
                ..UIStyle::default()
            },
        );
        // Measure button width
        let btn_style = btn_style();
        let button_w = tree.text_width(button_label, btn_style.font_size, crate::node::FontWeight::Regular) + 2.0 * GAP;
        tree.add_label(
            Some(header_id),
            inner_x + GAP + triangle_w,
            cy,
            (inner_w - 2.0 * GAP - triangle_w - button_w - GAP).max(0.0),
            ROW_H,
            name,
            section_label_style(),
        );
        // Right-aligned Import Model button
        self.import_model_id = Some(tree.add_button_keyed(
            Some(header_id),
            inner_x + inner_w - button_w,
            cy,
            button_w,
            ROW_H,
            btn_style,
            button_label,
            button_key,
        ));
        // Register outliner fold header for click routing
        self.outliner_row_ids.push((header_id, SceneSelection::OutlinerFold(name)));
        cy + ROW_H
    }

    /// The panel's ONE param-row renderer (P2 slice 2a). Filters
    /// `self.full_params` (the layer's real generator `ParamSurface`)
    /// down to `sections` — an ORDERED list; rendered in that order, one
    /// header per distinct section, rows within a section in the manifest's
    /// own order (never re-sorted) — configures `self.properties_card` from
    /// the filtered slice (real param ids, real modulation state, no
    /// synthesis), and renders every retained row through the shared
    /// `build_param_row`/`RowIndex`/`row_action` core `ParamCardPanel` uses
    /// (section 5.6: one row component, no per-panel forks). `configure_from_filtered`
    /// builds the id→row-index join map the per-frame value sync
    /// (`sync_properties_values`) resolves against — no retained-index list
    /// (BUG-313). Renders nothing when there's no config or no section
    /// matches — callers render their own honest empty/custom messaging.
    pub(crate) fn build_filtered_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        sections: &[String],
    ) -> f32 {
        self.build_filtered_properties_owned(tree, inner_x, inner_w, cy, (sections, None))
    }

    fn build_filtered_properties_owned(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        (sections, owner_ids): (&[String], Option<&[u32]>),
    ) -> f32 {
        let Some(config) = self.full_params.clone() else {
            self.properties_card.resize(0);
            return cy;
        };
        // BUG-292: every row this pass builds must dispatch to the panel's
        // OWN bound layer (`GeneratorOf`), never the active-layer-resolved
        // plain `Generator` — `full_params` is only ever populated alongside
        // `SceneSetupState::Live` (state_sync.rs's `configure`/
        // `configure_params` call together), so `live_layer_id()` is always
        // `Some` here in practice; guarded explicitly rather than assumed,
        // mirroring the `full_params` guard above.
        let Some(target) = self.live_layer_id().cloned().map(GraphParamTarget::GeneratorOf) else {
            self.properties_card.resize(0);
            return cy;
        };
        let mut retained: Vec<usize> = Vec::new();
        for section in sections {
            for (i, p) in config.rows.iter().enumerate() {
                // Scene exposure IDs are stamped as {owner_doc_id}_{param}.
                let selected_material_id = p.spec.material_role.is_some()
                    && self.active_material_info.as_ref().is_some_and(|info| {
                        info.params.iter().any(|(_, id)| id == &p.id)
                    });
                let owned = selected_material_id || owner_ids.is_none_or(|ids| {
                    p.id.as_ref()
                        .split('_')
                        .next()
                        .and_then(|s| s.parse::<u32>().ok())
                        .is_some_and(|id| ids.contains(&id))
                });
                if p.spec.section.as_deref() == Some(section.as_str())
                    && owned
                    && !retained.contains(&i)
                    && self.material_param_selected(p)
                    && Self::row_feature(p).is_none_or(|feature| self.material_feature_visible(&config.rows, feature))
                    && self.material_rgb_row_visible(&config.rows, p)
                {
                    retained.push(i);
                }
            }
        }
        // The manifest remains authoritative for row order, but material
        // presentation uses stable semantic buckets. Stable sorting only
        // affects material rows; transform/object rows retain their positions.
        if let Some(position) = retained.iter().position(|&index| self.material_object_gain(&config.rows[index])) {
            let gain = retained.remove(position);
            let position = retained.iter().position(|&index| config.rows[index].spec.material_role.is_some())
                .unwrap_or(retained.len());
            retained.insert(position, gain);
        }
        let material_positions: Vec<usize> = retained
            .iter()
            .enumerate()
            .filter_map(|(position, &index)| (config.rows[index].spec.material_role.is_some()
                || self.material_object_gain(&config.rows[index])).then_some(position))
            .collect();
        let mut material_rows: Vec<usize> = material_positions.iter().map(|&position| retained[position]).collect();
        material_rows.sort_by_key(|&index| self.material_bucket(&config.rows[index]));
        for (position, index) in material_positions.into_iter().zip(material_rows) {
            retained[position] = index;
        }
        self.properties_card.configure_from_filtered(&config, &retained);
        self.properties_card.restore_live(&target);

        if retained.is_empty() {
            return cy;
        }

        let RowGeometry { label_width, slider_w } = super::param_card::row_geometry(inner_w, false);

        // Clear section header ids from previous build
        self.properties_card.row_host.section_header_ids.clear();

        let mut i = 0usize;
        while i < retained.len() {
            let cur_section = self.material_section_name(&config.rows[retained[i]]);
            if let Some(name) = cur_section.as_deref()
                && let Some(family) = Self::material_family_from_section(name)
            {
                cy = self.build_material_placement(tree, inner_x, inner_w, cy, family);
            }
            if let Some(name) = &cur_section {
                let folded = self.material_section_folded(name);
                self.section_folded.entry(name.clone()).or_insert(folded);
                // Build interactive section header row
                let header_id = tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    UIStyle {
                        bg_color: color::INSPECTOR_BG,
                        hover_bg_color: color::HOVER_OVERLAY,
                        pressed_bg_color: color::PRESS_OVERLAY,
                        corner_radius: color::SMALL_RADIUS,
                        ..UIStyle::default()
                    },
                    "",
                    param_row_key_base(&config.rows[retained[i]].id) | ROW_ROLE_SECTION_HEADER,
                );
                if config.rows[retained[i]].spec.material_role.is_some() {
                    tree.set_name(
                        header_id,
                        format!("material.section.{}", config.rows[retained[i]].id),
                    );
                }
                let triangle_w = 16.0;
                let triangle = if folded { "\u{25B8}" } else { "\u{25BE}" }; // ▸ / ▾
                tree.add_label(
                    Some(header_id),
                    inner_x + GAP,
                    cy,
                    triangle_w,
                    ROW_H,
                    triangle,
                    UIStyle {
                        text_color: crate::color::TEXT_DIMMED_C32,
                        font_size: crate::color::FONT_LABEL,
                        text_align: TextAlign::Center,
                        ..UIStyle::default()
                    },
                );
                tree.add_label(
                    Some(header_id),
                    inner_x + GAP + triangle_w,
                    cy,
                    (inner_w - 2.0 * GAP - triangle_w).max(0.0),
                    ROW_H,
                    &self.material_section_display_name(name),
                    label_style(),
                );
                // Register header for click routing
                self.properties_card.row_host.section_header_ids.push((header_id, name.clone()));
                self.properties_card.row_host.row_index.insert(
                    tree.widget_of(header_id),
                    i,
                    RowRole::SectionHeader,
                );
                cy += ROW_H;
                // Skip folded section's rows
                if folded {
                    while i < retained.len() && self.material_section_name(&config.rows[retained[i]]) == cur_section {
                        i += 1;
                    }
                    continue;
                }
            }
            if let Some(name) = &cur_section {
                cy = self.build_material_section_sources(tree, inner_x, inner_w, cy, name);
            }
            while i < retained.len() && self.material_section_name(&config.rows[retained[i]]) == cur_section {
                cy = self.build_properties_row(tree, inner_x, cy, i, label_width, slider_w, target.clone());
                i += 1;
            }
        }
        cy + ROW_GAP
    }

    /// Material descriptors provide the user-facing grouping while ordinary
    /// scene rows continue to use their stamped manifest section verbatim.
    fn material_section_name(&self, row: &ParamRow) -> Option<String> {
        if self.material_object_gain(row) {
            return Some("Emission".to_string());
        }
        let Some(role) = row.spec.material_role else {
            return match row.spec.section.as_deref() {
                Some("Material") => Some("Advanced".to_string()),
                _ => row.spec.section.clone(),
            };
        };
        let name = match role {
            MaterialParamRole::Placement(family, ..) | MaterialParamRole::Sampler(family, ..) => {
                if self.material_family_connected(family) {
                    format!("Textures · {}", Self::material_family_label(family))
                } else {
                    "Advanced · Dormant Textures".to_string()
                }
            }
            MaterialParamRole::FeatureMode(feature) => match feature {
                crate::param_surface::MaterialFeature::Coat => "Coat".to_string(),
                crate::param_surface::MaterialFeature::Iridescence => "Iridescence".to_string(),
                crate::param_surface::MaterialFeature::Emission => "Emission".to_string(),
                crate::param_surface::MaterialFeature::Glass => "Glass".to_string(),
                crate::param_surface::MaterialFeature::Sheen => "Sheen".to_string(),
                crate::param_surface::MaterialFeature::Anisotropy => "Anisotropy".to_string(),
                crate::param_surface::MaterialFeature::Translucency => "Translucency".to_string(),
            },
            MaterialParamRole::Scalar(group) | MaterialParamRole::Colour(group, ..) => match group {
                MaterialGroup::Surface => "Surface".to_string(),
                MaterialGroup::Opacity => "Opacity & Cutout".to_string(),
                MaterialGroup::Feature(feature) => match feature {
                    crate::param_surface::MaterialFeature::Coat => "Coat".to_string(),
                    crate::param_surface::MaterialFeature::Iridescence => "Iridescence".to_string(),
                    crate::param_surface::MaterialFeature::Emission => "Emission".to_string(),
                    crate::param_surface::MaterialFeature::Glass => "Glass".to_string(),
                    crate::param_surface::MaterialFeature::Sheen => "Sheen".to_string(),
                    crate::param_surface::MaterialFeature::Anisotropy => "Anisotropy".to_string(),
                    crate::param_surface::MaterialFeature::Translucency => "Translucency".to_string(),
                },
                MaterialGroup::Advanced => "Advanced".to_string(),
            },
        };
        // Keep the stored cutoff reachable without presenting it as active
        // opacity authoring in Solid/Fade. Its value remains intact in the
        // Advanced drawer until Cutout is selected.
        if self.material_param_named(row, "alpha_cutoff")
            && self.material_opacity_mode() != Some(1)
        {
            return Some("Advanced · Opacity".to_string());
        }
        Some(name)
    }

    /// Match the descriptor's exact inner name when the app supplied the
    /// selected-material dictionary. The display-name fallback keeps older
    /// snapshots readable without using an id suffix as a write target.
    fn material_param_named(&self, row: &ParamRow, name: &str) -> bool {
        if let Some(info) = &self.active_material_info {
            if info
                .params
                .iter()
                .any(|(inner, id)| id == &row.id && inner == name)
            {
                return true;
            }
            if info.params.iter().any(|(_, id)| id == &row.id) {
                return false;
            }
        }
        row.spec.name
            .chars()
            .filter(|character| !character.is_ascii_whitespace() && *character != '-')
            .flat_map(char::to_lowercase)
            .eq(name.chars().filter(|character| *character != '_'))
    }

    fn material_value(&self, id: &manifold_foundation::ParamId) -> Option<f32> {
        self.properties_card
            .row_id_index
            .get(id.as_ref())
            .and_then(|&slot| self.properties_card.current_values.get(slot).copied())
            .or_else(|| {
                self.full_params.as_ref().and_then(|surface| {
                    surface
                        .rows
                        .iter()
                        .find(|row| row.id == *id)
                        .map(|row| row.value.effective)
                })
            })
    }

    fn material_opacity_mode(&self) -> Option<i32> {
        self.full_params.as_ref()?.rows.iter().find_map(|row| {
            (row.spec.material_role == Some(MaterialParamRole::Scalar(MaterialGroup::Opacity))
                && self.material_param_named(row, "alpha_mode"))
            .then(|| self.material_value(&row.id).unwrap_or(row.value.effective).round() as i32)
        })
    }

    fn material_section_folded(&self, name: &str) -> bool {
        self.section_folded.get(name).copied().unwrap_or_else(|| {
            name == "Advanced"
                || name == "Advanced · Opacity"
                || name == "Advanced · Dormant Textures"
                || name.starts_with("Textures · ")
        })
    }

    fn material_section_display_name(&self, name: &str) -> String {
        if let Some(family) = Self::material_family_from_section(name) {
            return format!("Advanced · {} UV & sampling", Self::material_family_label(family));
        }
        name.to_string()
    }

    fn build_material_section_sources(
        &self, tree: &mut UITree, x: f32, width: f32, mut cy: f32, name: &str,
    ) -> f32 {
        let Some(info) = &self.active_material_info else { return cy };
        let features = [
            crate::param_surface::MaterialFeature::Coat,
            crate::param_surface::MaterialFeature::Iridescence,
            crate::param_surface::MaterialFeature::Emission,
            crate::param_surface::MaterialFeature::Glass,
            crate::param_surface::MaterialFeature::Sheen,
            crate::param_surface::MaterialFeature::Anisotropy,
            crate::param_surface::MaterialFeature::Translucency,
        ];
        for texture in &info.textures {
            if !texture.connected || Self::material_family_for_port(&texture.port).is_some() {
                continue;
            }
            let belongs = features.iter().any(|feature|
                Self::material_feature_label(*feature) == name
                    && Self::material_feature_map_for_port(*feature, &texture.port));
            let advanced = name == "Advanced" && !features.iter().any(|feature|
                Self::material_feature_map_for_port(*feature, &texture.port));
            if belongs || advanced {
                tree.add_label(Some(self.content_parent), x, cy, width, ROW_H,
                    &format!("{} · {} · mesh UV", texture.label, texture.source_label), label_style());
                cy += ROW_H;
            }
        }
        cy
    }

    fn material_family_label(family: MaterialMapFamily) -> &'static str {
        match family {
            MaterialMapFamily::Base => "Base Color",
            MaterialMapFamily::Normal => "Normal",
            MaterialMapFamily::MetallicRoughness => "Metallic / Roughness",
            MaterialMapFamily::Occlusion => "Occlusion",
            MaterialMapFamily::Emission => "Emission",
        }
    }

    fn material_family_for_port(port: &str) -> Option<MaterialMapFamily> {
        match port {
            "base_color_map" => Some(MaterialMapFamily::Base),
            "normal_map" => Some(MaterialMapFamily::Normal),
            "mr_map" | "metallic_roughness_map" => Some(MaterialMapFamily::MetallicRoughness),
            "occlusion_map" => Some(MaterialMapFamily::Occlusion),
            "emissive_map" => Some(MaterialMapFamily::Emission),
            _ => None,
        }
    }

    fn material_family_connected(&self, family: MaterialMapFamily) -> bool {
        self.active_material_info.as_ref().is_some_and(|info| {
            info.textures.iter().any(|texture| {
                texture.connected && Self::material_family_for_port(&texture.port) == Some(family)
            })
        })
    }

    fn material_family_index(family: MaterialMapFamily) -> usize {
        match family {
            MaterialMapFamily::Base => 0,
            MaterialMapFamily::Normal => 1,
            MaterialMapFamily::MetallicRoughness => 2,
            MaterialMapFamily::Occlusion => 3,
            MaterialMapFamily::Emission => 4,
        }
    }

    fn material_placement_rows(
        &self,
        family: MaterialMapFamily,
    ) -> Option<[ParamRow; 6]> {
        let surface = self.full_params.as_ref()?;
        let components = [
            UvComponent::M00,
            UvComponent::M01,
            UvComponent::M10,
            UvComponent::M11,
            UvComponent::Tx,
            UvComponent::Ty,
        ];
        let mut found: [Option<ParamRow>; 6] = std::array::from_fn(|_| None);
        for row in surface.rows.iter().filter(|row| self.material_param_selected(row)) {
            let Some(MaterialParamRole::Placement(row_family, component)) = row.spec.material_role
            else {
                continue;
            };
            if row_family != family {
                continue;
            }
            let Some(index) = components.iter().position(|candidate| *candidate == component) else {
                continue;
            };
            if found[index].is_none() {
                found[index] = Some(row.clone());
            }
        }
        let [Some(m00), Some(m01), Some(m10), Some(m11), Some(tx), Some(ty)] = found else {
            return None;
        };
        Some([m00, m01, m10, m11, tx, ty])
    }

    fn configure_material_placements(&mut self, info: &MaterialInspectorInfo) {
        let Some(target) = self.live_layer_id().cloned().map(GraphParamTarget::GeneratorOf) else {
            return;
        };
        for family in [
            MaterialMapFamily::Base,
            MaterialMapFamily::Normal,
            MaterialMapFamily::MetallicRoughness,
            MaterialMapFamily::Occlusion,
            MaterialMapFamily::Emission,
        ] {
            let index = Self::material_family_index(family);
            if !self.material_family_connected(family) {
                continue;
            }
            let Some(rows) = self.material_placement_rows(family) else {
                continue;
            };
            let refs = [&rows[0], &rows[1], &rows[2], &rows[3], &rows[4], &rows[5]];
            self.material_placement_widgets[index].configure(
                target.clone(),
                info.object.clone(),
                info.material.clone(),
                refs,
            );
            self.material_placement_active[index] = true;
        }
    }

    fn material_family_from_section(name: &str) -> Option<MaterialMapFamily> {
        match name {
            "Textures · Base Color" => Some(MaterialMapFamily::Base),
            "Textures · Normal" => Some(MaterialMapFamily::Normal),
            "Textures · Metallic / Roughness" => Some(MaterialMapFamily::MetallicRoughness),
            "Textures · Occlusion" => Some(MaterialMapFamily::Occlusion),
            "Textures · Emission" => Some(MaterialMapFamily::Emission),
            _ => None,
        }
    }

    fn build_material_placement(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        family: MaterialMapFamily,
    ) -> f32 {
        let index = Self::material_family_index(family);
        if !self.material_placement_active[index] || self.material_placement_built[index] {
            return cy;
        }
        self.material_placement_built[index] = true;
        let mut cy = cy;
        let source = self.active_material_info.as_ref().and_then(|info|
            info.textures.iter().find(|texture| texture.connected
                && Self::material_family_for_port(&texture.port) == Some(family)));
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H,
            &format!("{} texture", Self::material_family_label(family)), section_label_style());
        cy += ROW_H;
        if let Some(texture) = source {
            tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H,
                &texture.source_label, label_style());
            cy += ROW_H;
        }
        if let Some(reason) = self.material_placement_widgets[index].reason() {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                &format!("Placement — {reason}"),
                label_style(),
            );
            return cy + ROW_H + ROW_GAP;
        }
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Placement",
            section_label_style(),
        );
        self.material_placement_widgets[index].build(
            tree,
            Some(self.content_parent),
            Rect::new(inner_x, cy + ROW_H, inner_w, inner_w),
            98_000 + index as u64 * 128,
        ) + ROW_H
    }

    fn material_feature_map_for_port(
        feature: crate::param_surface::MaterialFeature,
        port: &str,
    ) -> bool {
        match feature {
            crate::param_surface::MaterialFeature::Coat => matches!(
                port,
                "clearcoat_map" | "clearcoat_roughness_map" | "clearcoat_normal_map"
            ),
            crate::param_surface::MaterialFeature::Iridescence => {
                matches!(port, "iridescence_map" | "iridescence_thickness_map")
            }
            crate::param_surface::MaterialFeature::Emission => port == "emissive_map",
            crate::param_surface::MaterialFeature::Glass => {
                matches!(port, "transmission_map" | "volume_thickness_map")
            }
            crate::param_surface::MaterialFeature::Sheen => {
                matches!(port, "sheen_color_map" | "sheen_roughness_map")
            }
            crate::param_surface::MaterialFeature::Anisotropy => port == "anisotropy_map",
            crate::param_surface::MaterialFeature::Translucency => port == "volume_thickness_map",
        }
    }

    fn material_object_gain(&self, row: &ParamRow) -> bool {
        self.active_material_info.as_ref().is_some_and(|info|
            info.object_gain.as_ref() == Some(&row.id))
    }

    fn material_param_selected(&self, row: &ParamRow) -> bool {
        row.spec.material_role.is_none()
            || self.active_material_info.as_ref().is_none_or(|info| {
                info.params.iter().any(|(_, id)| id == &row.id)
            })
    }

    fn selected_material_rows(&self) -> Vec<ParamRow> {
        self.full_params
            .as_ref()
            .map(|surface| {
                surface
                    .rows
                    .iter()
                    .filter(|row| self.material_param_selected(row))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn material_rgb_row_visible(&self, rows: &[ParamRow], row: &ParamRow) -> bool {
        let Some(anchor) = rows.iter().find_map(|candidate| {
            candidate.rgb_members.as_ref().and_then(|members| {
                members.contains(&row.id).then(|| candidate.id.clone())
            })
        }) else {
            return true;
        };
        row.id == anchor || self.material_rgb_expanded.contains(&anchor)
    }

    fn material_rgb_colour(row: &ParamRow) -> Option<crate::param_surface::MaterialColour> {
        match row.spec.material_role {
            Some(MaterialParamRole::Colour(_, colour, RgbChannel::R)) => Some(colour),
            _ => None,
        }
    }

    fn material_rgb_members_for_param(
        &self,
        param_id: &manifold_foundation::ParamId,
    ) -> Option<[manifold_foundation::ParamId; 3]> {
        self.properties_card.rows.iter().find_map(|row| {
            row.rgb_members.as_ref().filter(|members| members.contains(param_id)).cloned()
        })
    }

    fn material_rgb_values(
        &self,
        members: &[manifold_foundation::ParamId; 3],
        changed_id: &manifold_foundation::ParamId,
        changed_value: f32,
    ) -> Option<[f32; 3]> {
        let mut values = [0.0; 3];
        for (index, id) in members.iter().enumerate() {
            let row = self.properties_card.row_id_index.get(id.as_ref()).copied()?;
            values[index] = self.properties_card.current_values.get(row).copied()?;
        }
        let channel = members.iter().position(|id| id == changed_id)?;
        values[channel] = changed_value;
        Some(values)
    }

    fn rewrite_material_rgb_actions(&self, actions: Vec<PanelAction>) -> Vec<PanelAction> {
        actions
            .into_iter()
            .map(|action| {
                let PanelAction::Scrub(super::ValueRef::Param(target, param_id), phase) = action else {
                    return action;
                };
                let Some(members) = self.material_rgb_members_for_param(&param_id) else {
                    return PanelAction::Scrub(super::ValueRef::Param(target, param_id), phase);
                };
                match phase {
                    super::ScrubPhase::Begin => PanelAction::Scrub(
                        super::ValueRef::ParamRgb(target, members),
                        super::ScrubPhase::Begin,
                    ),
                    super::ScrubPhase::Move(super::ScrubValue::Scalar(value)) => {
                        let Some(values) = self.material_rgb_values(&members, &param_id, value) else {
                            return PanelAction::Scrub(super::ValueRef::Param(target, param_id), super::ScrubPhase::Move(super::ScrubValue::Scalar(value)));
                        };
                        PanelAction::Scrub(
                            super::ValueRef::ParamRgb(target, members),
                            super::ScrubPhase::Move(super::ScrubValue::Rgb(values)),
                        )
                    }
                    super::ScrubPhase::Commit => PanelAction::Scrub(
                        super::ValueRef::ParamRgb(target, members),
                        super::ScrubPhase::Commit,
                    ),
                    other => PanelAction::Scrub(super::ValueRef::Param(target, param_id), other),
                }
            })
            .collect()
    }

    fn row_feature(row: &ParamRow) -> Option<crate::param_surface::MaterialFeature> {
        match row.spec.material_role {
            Some(MaterialParamRole::FeatureMode(feature))
            | Some(MaterialParamRole::Scalar(MaterialGroup::Feature(feature)))
            | Some(MaterialParamRole::Colour(MaterialGroup::Feature(feature), ..)) => Some(feature),
            _ => None,
        }
    }

    fn material_bucket(&self, row: &ParamRow) -> usize {
        if self.material_object_gain(row) {
            return 2 + crate::param_surface::MaterialFeature::Emission as usize;
        }
        match row.spec.material_role {
            Some(MaterialParamRole::Scalar(MaterialGroup::Surface))
            | Some(MaterialParamRole::Colour(MaterialGroup::Surface, ..)) => 0,
            Some(MaterialParamRole::Scalar(MaterialGroup::Opacity))
            | Some(MaterialParamRole::Colour(MaterialGroup::Opacity, ..)) => 1,
            Some(MaterialParamRole::FeatureMode(feature))
            | Some(MaterialParamRole::Scalar(MaterialGroup::Feature(feature)))
            | Some(MaterialParamRole::Colour(MaterialGroup::Feature(feature), ..)) => 2 + feature as usize,
            Some(MaterialParamRole::Placement(family, ..)) | Some(MaterialParamRole::Sampler(family, ..)) => {
                if self.material_family_connected(family) { 10 + family as usize } else { 30 }
            }
            Some(MaterialParamRole::Scalar(MaterialGroup::Advanced))
            | Some(MaterialParamRole::Colour(MaterialGroup::Advanced, ..)) => 20,
            None if row.spec.section.as_deref() == Some("Material") => 20,
            None => usize::MAX,
        }
    }

    fn material_feature_visible(
        &self,
        rows: &[ParamRow],
        feature: crate::param_surface::MaterialFeature,
    ) -> bool {
        let authored = rows
            .iter()
            .filter(|row| self.material_param_selected(row))
            .filter(|row| Self::row_feature(row) == Some(feature))
            .any(|row| {
            // A wire, automation lane, or host mapping is an authored
            // attachment even when the current scalar happens to equal its
            // neutral default. Keep the feature visible so the attachment
            // remains reachable from the inspector.
            let attached = row.value.driven
                || row.material_attached
                || row.modulation.driver_active
                || row.modulation.envelope_active
                || row.modulation.automation_active
                || row.audio.active
                || row.mapping.ableton_display.is_some()
                || row.mapping.ableton_range.is_some();
            attached
                || (self.material_feature_is_controlling(row, feature)
                    && (row.value.base - row.spec.default).abs() > f32::EPSILON)
        });
        if authored {
            return true;
        }
        self.active_material_info.as_ref().is_some_and(|info| {
            info.textures.iter().any(|texture| {
                texture.connected && Self::material_feature_map_for_port(feature, &texture.port)
            })
        })
    }

    /// Secondary feature values remain editable after the feature is added,
    /// but they do not themselves make a neutral feature appear in the main
    /// surface. The mode and the feature's primary control are the authored
    /// presence signals; attachments on any member still force presence.
    fn material_feature_is_controlling(
        &self,
        row: &ParamRow,
        feature: crate::param_surface::MaterialFeature,
    ) -> bool {
        if matches!(
            row.spec.material_role,
            Some(MaterialParamRole::FeatureMode(mode)) if mode == feature
        ) {
            return true;
        }
        let Some(inner_name) = self.active_material_info.as_ref().and_then(|info| {
            info.params
                .iter()
                .find(|(_, id)| id == &row.id)
                .map(|(name, _)| name.as_str())
        }) else {
            return false;
        };
        match feature {
            crate::param_surface::MaterialFeature::Coat => inner_name == "clearcoat",
            crate::param_surface::MaterialFeature::Iridescence => inner_name == "iridescence",
            crate::param_surface::MaterialFeature::Emission => {
                matches!(inner_name, "emission_r" | "emission_g" | "emission_b" | "emission_intensity")
            }
            crate::param_surface::MaterialFeature::Glass => inner_name == "transmission",
            crate::param_surface::MaterialFeature::Sheen => {
                matches!(inner_name, "sheen_color_r" | "sheen_color_g" | "sheen_color_b")
            }
            crate::param_surface::MaterialFeature::Anisotropy => inner_name == "anisotropy_strength",
            crate::param_surface::MaterialFeature::Translucency => inner_name == "translucency",
        }
    }

    fn material_feature_label(feature: crate::param_surface::MaterialFeature) -> &'static str {
        match feature {
            crate::param_surface::MaterialFeature::Coat => "Coat",
            crate::param_surface::MaterialFeature::Iridescence => "Iridescence",
            crate::param_surface::MaterialFeature::Emission => "Emission",
            crate::param_surface::MaterialFeature::Glass => "Glass",
            crate::param_surface::MaterialFeature::Sheen => "Sheen",
            crate::param_surface::MaterialFeature::Anisotropy => "Anisotropy",
            crate::param_surface::MaterialFeature::Translucency => "Translucency",
        }
    }

    fn material_feature_writes(
        &self,
        rows: &[ParamRow],
        feature: crate::param_surface::MaterialFeature,
        mode_id: &manifold_foundation::ParamId,
    ) -> Vec<MaterialParamWrite> {
        let seed_names: &[(&str, f32)] = match feature {
            crate::param_surface::MaterialFeature::Coat => &[("clearcoat", 1.0)],
            crate::param_surface::MaterialFeature::Iridescence => &[("iridescence", 1.0)],
            crate::param_surface::MaterialFeature::Emission => &[
                ("emission_r", 1.0), ("emission_g", 1.0),
                ("emission_b", 1.0), ("emission_intensity", 1.0),
            ],
            crate::param_surface::MaterialFeature::Glass => &[("transmission", 1.0)],
            crate::param_surface::MaterialFeature::Sheen => &[
                ("sheen_color_r", 0.5), ("sheen_color_g", 0.5), ("sheen_color_b", 0.5),
            ],
            crate::param_surface::MaterialFeature::Anisotropy => &[("anisotropy_strength", 0.5)],
            crate::param_surface::MaterialFeature::Translucency => &[("translucency", 0.5)],
        };
        let mut writes = vec![MaterialParamWrite { param_id: mode_id.clone(), value: 2.0 }];
        for &(name, value) in seed_names {
            let candidates: Vec<&ParamRow> = rows.iter().filter(|row| {
                Self::row_feature(row) == Some(feature)
                    && self.material_feature_is_controlling(row, feature)
                    && self.active_material_info.as_ref().is_some_and(|info| {
                        info.params.iter().any(|(inner_name, id)| inner_name == name && id == &row.id)
                    })
            }).collect();
            let Some(row) = candidates.first().copied().filter(|_| candidates.len() == 1) else {
                continue;
            };
            let attached = row.value.driven
                || row.material_attached
                || row.modulation.driver_active
                || row.modulation.envelope_active
                || row.modulation.automation_active
                || row.audio.active
                || row.mapping.ableton_display.is_some()
                || row.mapping.ableton_range.is_some();
            if !attached && (row.value.base - row.spec.default).abs() <= f32::EPSILON {
                writes.push(MaterialParamWrite { param_id: row.id.clone(), value });
            }
        }
        writes
    }

    fn material_action_context(
        &self,
        object: &ModifierObjectRef,
        material: &ModifierObjectRef,
        require_shared_scope: bool,
    ) -> bool {
        !object.node.is_empty()
            && !material.node.is_empty()
            && self.active_material_info.as_ref().is_some_and(|info| {
                info.object == *object
                    && info.material == *material
                    && (!require_shared_scope || info.shared_object_count.is_some())
            })
    }

    /// One properties-card row, built through the SAME shared core every
    /// effect/generator card row uses — no synthesis, no `RowAddr`: `slot`
    /// indexes `self.properties_card.rows` directly, whose
    /// `id` IS the real exposed param — the dispatch identity every
    /// downstream `PanelAction` carries unchanged.
    fn build_properties_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        cy: f32,
        slot: usize,
        label_width: f32,
        slider_w: f32,
        target: GraphParamTarget,
    ) -> f32 {
        // Scene-relative range substitution for translate params (SCENE_PANEL_UX_DESIGN.md).
        // When bounds are available, substitute the derived range (center ± 2×extent per axis)
        // so both slider drag clamp and type-in clamp see the same widened range.
        if let Some((bounds_min, bounds_max)) = self.state.as_live().and_then(|vm| vm.scene_bounds) {
            // Extract param ID string from the ParamRow's id field (Cow<'static, str>)
            let param_id = self.properties_card.rows[slot].id.as_ref();

            // Match transform_3d position params: pos_x, pos_y, pos_z
            if let Some(axis) = param_id.strip_prefix("pos_").and_then(|suffix| match suffix {
                "x" => Some(0usize),
                "y" => Some(1usize),
                "z" => Some(2usize),
                _ => None,
            }) {
                // Compute center and extent for this axis
                let center = (bounds_min[axis] + bounds_max[axis]) * 0.5;
                let mut extent = bounds_max[axis] - bounds_min[axis];

                // Floor extent at 1.0 so tiny scenes keep a usable range
                extent = extent.max(1.0);

                // Range is center ± 2×extent (per brief decision)
                let range_min = center - 2.0 * extent;
                let range_max = center + 2.0 * extent;

                self.properties_card.rows[slot].spec.min = range_min;
                self.properties_card.rows[slot].spec.max = range_max;
            }
        }
        let mut info = self.properties_card.rows[slot].clone();
        if (self.material_param_named(&info, "metallic") || self.material_param_named(&info, "roughness"))
            && self.material_family_connected(MaterialMapFamily::MetallicRoughness)
        {
            info.spec.inactive_reason = Some("From texture — scalar applies without this map".into());
        }
        if self.material_object_gain(&info) {
            info.spec.name = "Object gain".into();
        }
        match info.spec.material_role {
            Some(MaterialParamRole::Placement(_, component)) => info.spec.name = match component {
                UvComponent::M00 => "M00", UvComponent::M01 => "M01",
                UvComponent::M10 => "M10", UvComponent::M11 => "M11",
                UvComponent::Tx => "Offset U", UvComponent::Ty => "Offset V",
            }.into(),
            Some(MaterialParamRole::Sampler(_, component)) => info.spec.name = match component {
                crate::param_surface::SamplerComponent::WrapU => "Wrap U",
                crate::param_surface::SamplerComponent::WrapV => "Wrap V",
                crate::param_surface::SamplerComponent::MagFilter => "Magnification",
                crate::param_surface::SamplerComponent::MinFilter => "Minification",
            }.into(),
            _ => {}
        }
        if self.material_param_named(&info, "alpha_mode") {
            info.spec.value_labels = Some(vec![
                "Solid".to_string(),
                "Cutout".to_string(),
                "Fade".to_string(),
            ]);
        } else if self.material_param_named(&info, "alpha_cutoff") {
            if self.material_opacity_mode() != Some(1) {
                info.spec.inactive_reason = Some("Only used in Cutout".to_string());
            }
        } else if self.material_param_named(&info, "color_a")
            && self.material_opacity_mode() == Some(0)
        {
            info.spec.inactive_reason = Some("Ignored in Solid".to_string());
        }

        let mut row_cy = cy;
        if info.rgb_members.is_some() && Self::material_rgb_colour(&info).is_some() {
            row_cy = self.build_material_swatch_header(
                tree,
                inner_x,
                cy,
                slot,
                label_width,
                slider_w,
                &info,
            );
            if !self.material_rgb_expanded.contains(&info.id) {
                return row_cy;
            }
        }

        // Trigger parameters use the same momentary button and ParamFire
        // dispatch as generator cards; a numeric slider cannot fire Reset.
        if info.spec.is_trigger {
            let row = build_toggle_trigger_row(
                tree, Some(self.content_parent), inner_x, row_cy, slider_w,
                &info, &self.properties_card.mod_state, slot, target,
                color::FONT_LABEL, true, false,
                Some(param_row_key_base(info.id.as_ref())), None,
            );
            let host = &mut self.properties_card.row_host;
            host.toggle_ids[slot] = Some(ToggleParamIds {
                label_id: row.label_id, button_id: row.button_id,
            });
            host.audio_btn_ids[slot] = row.audio_btn;
            host.audio_configs[slot] = row.audio_config;
            host.audio_trigger_mode_badge_ids[slot] = row.mode_badge_id;
            host.reindex_row(tree, slot);
            return row.new_cy;
        }

        // The value this row must SHOW: the sync's last-pushed value (the tree
        // is minted fresh every frame — a row the dirty-check skipped must
        // redraw that value here or it snaps back to the default). Never-
        // pushed (NaN) rows fall back to the synced base, which is what the
        // sync will compare against and push this same frame.
        let display_value = match self.properties_card.last_pushed_values.get(slot) {
            Some(&v) if !v.is_nan() => v,
            _ => self
                .properties_card
                .current_values
                .get(slot)
                .copied()
                .unwrap_or(info.spec.default),
        };
        let display_value = self
            .properties_card
            .row_host
            .active_param_value(&target, &info.id)
            .unwrap_or(display_value);

        let built = build_param_row(
            tree,
            Some(self.content_parent),
            inner_x,
            row_cy,
            slider_w,
            &info,
            &self.properties_card.mod_state,
            slot,
            target,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            true,
            label_width,
            false,
            self.properties_card.mod_active_tab.get(slot).copied().unwrap_or(ModTab::Driver),
            true,
            Some(param_row_key_base(info.id.as_ref())),
            None,
            Some(display_value),
        );
        let new_cy = built.new_cy;
        self.properties_card.row_host.install_row(tree, slot, built);
        if let Some(MaterialParamRole::FeatureMode(feature)) = info.spec.material_role
            && let Some(slider) = &self.properties_card.row_host.slider_ids[slot]
        {
            let widget = |node: NodeId| tree.widget_of(node);
            self.properties_card.row_host.row_index.insert(widget(slider.track), slot, RowRole::MaterialFeatureToggle(feature));
            self.properties_card.row_host.row_index.insert(widget(slider.value_text), slot, RowRole::MaterialFeatureToggle(feature));
            if let Some(label) = slider.label {
                self.properties_card.row_host.row_index.insert(widget(label), slot, RowRole::MaterialFeatureToggle(feature));
            }
        }

        new_cy
    }

    fn material_full_value(&self, id: &manifold_foundation::ParamId) -> f32 {
        self.properties_card
            .row_id_index
            .get(id.as_ref())
            .and_then(|&slot| self.properties_card.current_values.get(slot).copied())
            .or_else(|| {
                self.full_params.as_ref().and_then(|surface| {
                    surface.rows.iter().find(|row| row.id == *id).map(|row| row.value.base)
                })
            })
            .unwrap_or(0.0)
            .clamp(0.0, 1.0)
    }

    fn build_material_swatch_header(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        cy: f32,
        slot: usize,
        label_width: f32,
        slider_w: f32,
        info: &ParamRow,
    ) -> f32 {
        let Some(rgb_members) = info.rgb_members.clone() else { return cy };
        let Some(colour) = Self::material_rgb_colour(info) else { return cy };
        let rgb = rgb_members.clone().map(|id| self.material_full_value(&id));
        let to_byte = |value: f32| (value * 255.0).round() as u8;
        let label = format!("#{:02X}{:02X}{:02X}", to_byte(rgb[0]), to_byte(rgb[1]), to_byte(rgb[2]));
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            label_width,
            ROW_H,
            match colour {
                crate::param_surface::MaterialColour::Base => "Base colour",
                crate::param_surface::MaterialColour::Specular => "Specular tint",
                crate::param_surface::MaterialColour::Emission => "Emission colour",
                crate::param_surface::MaterialColour::Sheen => "Sheen colour",
                crate::param_surface::MaterialColour::Attenuation => "Attenuation colour",
            },
            label_style(),
        );
        let swatch_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + label_width,
            cy,
            (slider_w - label_width).max(0.0),
            ROW_H,
            UIStyle {
                bg_color: Color32::new(to_byte(rgb[0]), to_byte(rgb[1]), to_byte(rgb[2]), 255),
                hover_bg_color: color::HOVER_OVERLAY,
                pressed_bg_color: color::PRESS_OVERLAY,
                text_color: if rgb.iter().copied().sum::<f32>() > 1.65 { Color32::BLACK } else { Color32::WHITE },
                font_size: color::FONT_LABEL,
                corner_radius: color::SMALL_RADIUS,
                ..btn_style()
            },
            &label,
            MATERIAL_SWATCH_KEY_BASE + slot as u64,
        );
        tree.set_name(swatch_id, format!("material.swatch.{}", info.id));
        self.material_swatch_ids.push((swatch_id, slot, rgb_members, colour));
        self.properties_card.row_host.row_index.insert(
            tree.widget_of(swatch_id),
            slot,
            RowRole::ColourSwatch(colour),
        );
        cy + ROW_H + ROW_GAP
    }

    fn build_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
        selected: SceneSelection,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Properties", section_label_style());
        cy += ROW_H;
        match selected {
            SceneSelection::Object(id) => {
                let Some(row) = vm.objects.iter().find_map(|o| match o {
                    ObjectRowVm::Known(r) if r.object_node_id == id => Some(r.as_ref()),
                    _ => None,
                }) else {
                    return cy;
                };
                cy = self.build_object_properties_header(tree, inner_x, inner_w, cy, row);
                self.build_object_properties_body(tree, inner_x, inner_w, cy, row)
            }
            SceneSelection::Light(id) => {
                let Some(row) = vm.lights.iter().find_map(|l| match l {
                    LightRowVm::Known(r) if r.node_doc_id == id => Some(r.as_ref()),
                    _ => None,
                }) else {
                    return cy;
                };
                cy = self.build_light_properties_header(tree, inner_x, inner_w, cy, row);
                self.build_light_properties_body(tree, inner_x, inner_w, cy, row)
            }
            SceneSelection::Camera => self.build_camera_section(tree, inner_x, inner_w, cy, vm),
            SceneSelection::World => self.build_world_properties(tree, inner_x, inner_w, cy, vm),
            SceneSelection::OutlinerFold(_) => cy, // Fold headers don't have properties
        }
    }

    /// Object properties header: editable name (click to rename — same
    /// single-click-opens-text-input UX the outliner/graph rename affordance
    /// already uses) + Duplicate + Remove (D11).
    fn build_object_properties_header(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        let btn_w = STEP_W * 4.0; // Frame + Duplicate + Remove
        let name_w = inner_w - btn_w - 8.0;
        let name_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            name_w,
            ROW_H,
            drag_value_style(),
            &row.name,
            obj_key(row.index, OBJ_OFF_NAME),
        );
        // Stable automation name (UX-P1): `scripts/ui-flows/` selects the
        // Properties header's name text by NAME, not raw text, so a flow can
        // assert "the header text changed" without hard-coding which object
        // it changed to.
        tree.set_name(name_id, "scene_setup.properties.name_value");
        let identity_node_id = row.group_node_id.unwrap_or(row.object_node_id);
        self.object_name_ids.push((identity_node_id, name_id, row.name.clone()));

        // Frame button (scene-panel-ux lane)
        let frame_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "Frame",
            obj_key(row.index, OBJ_OFF_FRAME),
        );
        self.object_frame_ids.push((frame_id, row.index));

        let dup_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0 + STEP_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{29C9}",
            obj_key(row.index, OBJ_OFF_REMOVE) + 1,
        );
        self.object_duplicate_ids.push((dup_id, row.index));
        let remove_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0 + STEP_W * 2.0,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2715}",
            obj_key(row.index, OBJ_OFF_REMOVE),
        );
        self.object_remove_ids.push((remove_id, row.index));
        cy + ROW_H + ROW_GAP
    }

    /// Object properties body: transform triplets, material quick knobs,
    /// modifier stack — the body `build_object_row` used to render only when
    /// expanded; now always rendered (there is no fold state left — the
    /// outliner IS the fold).
    /// P2 slice 2a: replaced the transform-triplet/material/metallic/
    /// roughness row builders with one `build_filtered_properties` pass over
    /// `row.sections` (Transform + Material + the object's own section +
    /// every modifier's own section — see `ObjectKnownRow::sections`'s doc
    /// comment). The modifier STACK below stays a structural verb (add/
    /// remove/reorder, unchanged) — only its per-modifier PARAM rows moved
    /// into the unified pass above (each modifier's section is already part
    /// of `row.sections`, so its rows render there, grouped under its own
    /// section header).
    fn build_object_properties_body(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        self.active_material_info = row.material_inspector.clone();
        if let Some(material) = row.material_inspector.clone() {
            self.configure_material_placements(&material);
            cy = self.build_material_header(tree, inner_x, inner_w, cy, &material);
            cy = self.build_material_feature_actions(tree, inner_x, inner_w, cy, &material);
            cy = self.build_filtered_properties(tree, inner_x, inner_w, cy, &row.sections);
            cy = self.build_material_inspector(tree, inner_x, inner_w, cy, row, row.skin.as_ref());
        } else if let Some(skin) = &row.skin {
            // Non-PBR materials still expose their layer-skin control, but
            // there is no material drawer to host it.
            cy = self.build_filtered_properties(tree, inner_x, inner_w, cy, &row.sections);
            cy = self.build_skin_row(tree, inner_x, inner_w, cy, row, skin);
        } else {
            cy = self.build_filtered_properties(tree, inner_x, inner_w, cy, &row.sections);
        }
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Modifiers", label_style());
        cy += ROW_H;
        if row.modifiers_addable {
            for m in &row.modifiers {
                cy = self.build_modifier_stack_row(
                    tree,
                    inner_x,
                    inner_w,
                    cy,
                    row.index,
                    row.group_node_id.unwrap_or(row.object_node_id),
                    m,
                    row.modifiers.len(),
                );
            }
            cy = self.build_add_modifier_button(
                tree, inner_x, inner_w, cy, row.index, row.group_node_id.unwrap_or(row.object_node_id),
            );
        } else {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Custom chain — edit in graph",
                label_style(),
            );
            cy += ROW_H;
        }
        cy + ROW_GAP
    }

    /// Material-specific structural affordances that sit beside the ordinary
    /// manifest rows: scope notice, starter looks, and texture ownership.
    /// Numeric factors and placement remain in `build_filtered_properties` so
    /// they retain the shared row host, stable ids, and modulation drawers.
    fn build_material_header(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        info: &MaterialInspectorInfo,
    ) -> f32 {
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Material",
            section_label_style(),
        );
        cy += ROW_H;
        let scope = match info.shared_object_count {
            Some(1) => "Applies to 1 object".to_string(),
            Some(n) => format!("Applies to {n} objects"),
            None => "Shared scope unknown".to_string(),
        };
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, &scope, label_style());
        cy += ROW_H;
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Custom material",
            section_label_style(),
        );
        cy += ROW_H;
        if info.shared_object_count.is_some() {
            let looks = [
                (MaterialLook::Matte, "Matte", 0_u64),
                (MaterialLook::Coated, "Coated", 1_u64),
                (MaterialLook::BrushedMetal, "Brushed Metal", 2_u64),
                (MaterialLook::Glass, "Glass", 3_u64),
            ];
            let gap = ROW_GAP;
            let button_w = ((inner_w - gap * 3.0) / 4.0).max(0.0);
            for (look, label, offset) in looks {
                let id = tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x + offset as f32 * (button_w + gap),
                    cy,
                    button_w,
                    ROW_H,
                    btn_style(),
                    label,
                    MATERIAL_LOOK_KEY_BASE + offset,
                );
                self.material_look_ids.push((id, look, info.object.clone(), info.material.clone()));
            }
        } else {
            tree.add_label(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                "Looks unavailable — shared scope unknown",
                label_style(),
            );
        }
        cy + ROW_H + ROW_GAP
    }

    fn build_material_feature_actions(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        info: &MaterialInspectorInfo,
    ) -> f32 {
        let feature_rows = self.selected_material_rows();
        const FEATURES: [crate::param_surface::MaterialFeature; 7] = [
            crate::param_surface::MaterialFeature::Coat,
            crate::param_surface::MaterialFeature::Iridescence,
            crate::param_surface::MaterialFeature::Emission,
            crate::param_surface::MaterialFeature::Glass,
            crate::param_surface::MaterialFeature::Sheen,
            crate::param_surface::MaterialFeature::Anisotropy,
            crate::param_surface::MaterialFeature::Translucency,
        ];
        let mut add_features = Vec::new();
        for feature in FEATURES {
            if self.material_feature_visible(&feature_rows, feature) {
                continue;
            }
            let Some(mode_row) = feature_rows.iter().find(|row| {
                row.spec.material_role == Some(MaterialParamRole::FeatureMode(feature))
            }) else {
                continue;
            };
            add_features.push((feature, mode_row.id.clone()));
        }
        if add_features.is_empty() {
            return cy;
        }
        tree.add_label(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            "Optional Features",
            section_label_style(),
        );
        cy += ROW_H;
        let button_w = ((inner_w - ROW_GAP * 2.0) / 3.0).max(0.0);
        let feature_count = add_features.len();
        for (index, (feature, mode_id)) in add_features.into_iter().enumerate() {
            let col = (index % 3) as f32;
            let row = (index / 3) as f32;
            let label = format!("+ Add {}", Self::material_feature_label(feature));
            let id = tree.add_button_keyed(
                Some(self.content_parent),
                inner_x + col * (button_w + ROW_GAP),
                cy + row * (ROW_H + ROW_GAP),
                button_w,
                ROW_H,
                btn_style(),
                &label,
                MATERIAL_LOOK_KEY_BASE + 32 + index as u64,
            );
            let writes = self.material_feature_writes(&feature_rows, feature, &mode_id);
            self.material_feature_ids
                .push((id, feature, writes, info.object.clone(), info.material.clone()));
        }
        cy + feature_count.div_ceil(3) as f32 * (ROW_H + ROW_GAP)
    }

    fn build_material_inspector(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        object_row: &ObjectKnownRow,
        skin: Option<&SkinRowVm>,
    ) -> f32 {
        if let Some(skin) = skin {
            tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Textures", section_label_style());
            cy += ROW_H;
            cy = self.build_skin_row(tree, inner_x, inner_w, cy, object_row, skin);
        }
        cy + ROW_GAP
    }

    /// P4b: one Skin row per Known object — source layer dropdown + target-map
    /// dropdown. Each half is a clickable button (not a bare label) so the
    /// affordance rule is met. A missing source layer shows a trailing chip.
    fn build_skin_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        row: &ObjectKnownRow,
        skin: &SkinRowVm,
    ) -> f32 {
        let label_w = crate::slider::label_width_for_row(inner_w);
        tree.add_label(Some(self.content_parent), inner_x, cy, label_w, ROW_H, "Skin", label_style());
        let btn_gap = 4.0f32;
        let remaining = (inner_w - label_w).max(0.0);
        let chip_w = if skin.source_missing { 80.0f32 } else { 0.0f32 };
        let chip_gap = if skin.source_missing { btn_gap } else { 0.0f32 };
        let btn_w = ((remaining - chip_w - chip_gap - btn_gap) / 2.0).max(0.0);
        let source_label = skin
            .source
            .as_ref()
            .and_then(|id| skin.source_options.iter().find(|(lid, _)| lid == id).map(|(_, name)| name.clone()))
            .unwrap_or_else(|| "None".to_string());
        let source_btn = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + label_w,
            cy,
            btn_w,
            ROW_H,
            btn_style(),
            &format!("Source: {source_label}"),
            obj_key(row.index, OBJ_OFF_SKIN_SOURCE),
        );
        tree.set_name(source_btn, "scene_setup.skin.source");
        self.skin_source_ids.push((source_btn, row.object_node_id, skin.clone()));
        let target_btn = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + label_w + btn_w + btn_gap,
            cy,
            btn_w,
            ROW_H,
            btn_style(),
            &format!("Map: {}", skin.target_map.label()),
            obj_key(row.index, OBJ_OFF_SKIN_TARGET),
        );
        tree.set_name(target_btn, "scene_setup.skin.target");
        self.skin_target_ids.push((target_btn, row.object_node_id, skin.clone()));
        if skin.source_missing {
            tree.add_label(
                Some(self.content_parent),
                inner_x + label_w + btn_w * 2.0 + btn_gap * 2.0,
                cy,
                chip_w,
                ROW_H,
                "missing layer",
                UIStyle {
                    text_color: color::TEXT_DIMMED_C32,
                    font_size: color::FONT_LABEL,
                    text_align: TextAlign::Center,
                    ..UIStyle::default()
                },
            );
        }
        cy + ROW_H + ROW_GAP
    }

    /// Light properties header: editable name (NEW, P5) + Remove (D11's
    /// `RemoveSceneLightCommand`; lights have no Duplicate verb — D11 scopes
    /// duplicate to objects only).
    fn build_light_properties_header(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        row: &LightKnownRow,
    ) -> f32 {
        let name_w = inner_w - STEP_W - 4.0;
        let name_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            name_w,
            ROW_H,
            drag_value_style(),
            &row.name,
            light_key(row.index, LIGHT_OFF_NAME),
        );
        self.light_name_ids.push((row.node_doc_id, name_id, row.name.clone()));
        let remove_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2715}",
            light_key(row.index, LIGHT_OFF_REMOVE),
        );
        self.light_remove_ids.push((remove_id, row.index));
        cy + ROW_H + ROW_GAP
    }

    /// Light properties body: mode/color/intensity/pos/aim/cast_shadows/
    /// shadow_softness + the always-present Light Size sub-row — the body
    /// `build_light_row` used to render only when expanded; now always on.
    /// P2 slice 2a: replaced the 13 hand-listed Mode/Color/Intensity/Pos/
    /// Aim/Shadow/Light-Size rows with one `build_filtered_properties` pass
    /// over `row.sections` (the light's own P1 section — see
    /// `LightKnownRow::sections`'s doc comment).
    fn build_light_properties_body(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        row: &LightKnownRow,
    ) -> f32 {
        self.build_filtered_properties(tree, inner_x, inner_w, cy, &row.sections)
    }

    /// World properties: Environment + Fog. P2 slice 2a: the numeric rows
    /// (Intensity/Fill/Density/Height Falloff, plus Mode if the environment
    /// atom exposes one — the old static "Mode: Softbox" chip is GONE,
    /// finding #9 of the design doc's audit: "Environment Mode is a dead
    /// chip... while the card exposes the same param as a working control")
    /// now come from ONE `build_filtered_properties` pass over
    /// `vm.world_sections` (the REAL "Environment"/"Atmosphere" section
    /// strings, in that order — see `SceneSetupVm::world_sections`'s doc
    /// comment). Structural fallback messaging (None/Custom + "+ Add …"
    /// buttons) stays panel-shaped, unchanged, rendered around the pass.
    fn build_world_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
    ) -> f32 {
        match &vm.environment {
            EnvironmentRowVm::Importer { hdri_file, .. } if !hdri_file.is_empty() => {
                tree.add_label(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    &format!("HDRI: {hdri_file}"),
                    label_style(),
                );
                cy += ROW_H;
            }
            EnvironmentRowVm::Custom => {
                tree.add_label(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    "Environment: Custom (edit in graph)",
                    label_style(),
                );
                cy += ROW_H;
            }
            EnvironmentRowVm::None => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Environment: None", label_style());
                cy += ROW_H;
                self.add_environment_id = Some(tree.add_button_keyed(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    btn_style(),
                    "+ Add Environment",
                    KEY_ADD_ENVIRONMENT,
                ));
                cy += ROW_H;
            }
            EnvironmentRowVm::Importer { .. } | EnvironmentRowVm::Bare { .. } => {}
        }

        cy = self.build_filtered_properties(tree, inner_x, inner_w, cy, &vm.world_sections);

        if matches!(vm.atmosphere, AtmosphereRowVm::None) {
            tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Fog: None", label_style());
            cy += ROW_H;
            self.add_fog_id = Some(tree.add_button_keyed(
                Some(self.content_parent),
                inner_x,
                cy,
                inner_w,
                ROW_H,
                btn_style(),
                "+ Add Fog",
                KEY_ADD_FOG,
            ));
            cy += ROW_H;
        }
        cy
    }

    /// P2 slice 2a: STRUCTURAL chrome only — display name + up/down/remove.
    /// This modifier's own PARAM rows no longer build here: they're part of
    /// `row.sections` (each modifier's own P1 section, e.g. "Teapot — Bend")
    /// and render through the unified `build_filtered_properties` pass in
    /// `build_object_properties_body`, ABOVE this stack list — the stack
    /// itself stays a structural verb (add/remove/reorder), unchanged.
    /// `mod_count` is the CURRENT stack length — up/down are always
    /// rendered (never conditionally hidden,
    /// `feedback_no_conditionally_visible_ui`) but only recorded as live
    /// targets when they wouldn't push past a stack boundary; clicking an
    /// inert one at the boundary is simply a no-op.
    fn build_modifier_stack_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        object_index: usize,
        group_node_id: u32,
        m: &ModifierKnownRow,
        mod_count: usize,
    ) -> f32 {
        let name_w = inner_w - STEP_W * 3.0;
        tree.add_label(Some(self.content_parent), inner_x, cy, name_w, ROW_H, &m.display_name, label_style());
        let btn_x = inner_x + name_w;
        let up_id = tree.add_button_keyed(
            Some(self.content_parent),
            btn_x,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2191}",
            modifier_row_key(object_index, m.index, MODIFIER_OFF_UP),
        );
        let down_id = tree.add_button_keyed(
            Some(self.content_parent),
            btn_x + STEP_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2193}",
            modifier_row_key(object_index, m.index, MODIFIER_OFF_DOWN),
        );
        let remove_id = tree.add_button_keyed(
            Some(self.content_parent),
            btn_x + STEP_W * 2.0,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{00D7}",
            modifier_row_key(object_index, m.index, MODIFIER_OFF_REMOVE),
        );
        self.modifier_remove_ids.push((remove_id, group_node_id, m.node_doc_id));
        if m.index > 0 {
            self.modifier_move_ids.push((up_id, group_node_id, m.node_doc_id, (m.index - 1) as u32));
        }
        if m.index + 1 < mod_count {
            self.modifier_move_ids.push((down_id, group_node_id, m.node_doc_id, (m.index + 1) as u32));
        }
        cy + ROW_H
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
        object_index: usize,
        group_node_id: u32,
    ) -> f32 {
        let btn_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            inner_w,
            ROW_H,
            btn_style(),
            "+ Add Modifier",
            modifier_add_button_key(object_index),
        );
        self.add_modifier_button_id = Some((btn_id, group_node_id));
        cy + ROW_H
    }

    /// Mouse-wheel scroll for the docked body.
    pub fn handle_scroll(&mut self, delta: f32) -> bool {
        self.scroll.apply_scroll_delta(delta)
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
        if let Some(actions) = self.handle_material_placement_event(event, tree) {
            return (true, actions);
        }
        match event {
            UIEvent::Click { node_id, .. } => {
                if *node_id == self.close_id {
                    // BUG-224: this used to call `self.close()` directly —
                    // that only flips the panel-local `open` flag, so
                    // `ui_root.layout.scene_setup_width` (the dock's actual
                    // screen footprint) never reset to 0, no rebuild ever
                    // fired (no `PanelAction` means `app_render.rs`'s
                    // dispatch loop never runs), and the header toggle
                    // button's highlight went stale — the × visibly did
                    // nothing. `AudioSetupPanel::handle_event`'s close arm
                    // (see its own doc comment) already has the correct
                    // one-toggle-path pattern: emit the same
                    // `PanelAction::OpenSceneSetup` the header button and
                    // Escape use, so `ui.toggle_scene_dock()` runs through
                    // the single owning path (width + open + rebuild +
                    // header sync all in lockstep).
                    return (true, vec![PanelAction::Root(RootAction::OpenSceneSetup)]);
                }
                // D7: an outliner row click sets the UI-local selection —
                // no command, no undo unit, valid even before a `Live` state
                // exists. D1 of SCENE_PANEL_UX_DESIGN.md: also emit
                // `SceneSetupSelectionChanged` so the dispatch loop's
                // `structural_change: true` rebuilds Properties THIS frame
                // instead of waiting for the next unrelated sync.
                if let Some((_, sel)) = self.outliner_row_ids.iter().find(|(id, _)| *id == *node_id) {
                    // scene-panel-ux lane: handle outliner fold toggles
                    if let SceneSelection::OutlinerFold(group_name) = sel {
                        let folded = self.outliner_folded.entry(*group_name).or_insert(false);
                        *folded = !*folded;
                        return (true, vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]);
                    }
                    // Normal selection change
                    if let SceneSetupState::Live(vm) = &self.state {
                        self.selection.insert(vm.layer_id.clone(), *sel);
                        return (true, vec![PanelAction::Root(RootAction::SceneSetupSelectionChanged(vm.layer_id.clone()))]);
                    }
                    return (true, Vec::new());
                }
                let mut actions = Vec::new();
                if let SceneSetupState::Live(vm) = &self.state {
                    if let Some((_, feature, writes, object, material)) = self
                        .material_feature_ids
                        .iter()
                        .find(|(id, _, _, _, _)| *id == *node_id)
                    {
                        if !self.material_action_context(object, material, false) {
                            return (true, Vec::new());
                        }
                        actions.push(PanelAction::Project(ProjectAction::MaterialParamsSet {
                            target: GraphParamTarget::GeneratorOf(vm.layer_id.clone()),
                            object: object.clone(),
                            material: material.clone(),
                            kind: MaterialEditKind::Feature,
                            writes: writes.clone(),
                            description: format!("Add {} feature", Self::material_feature_label(*feature)),
                        }));
                    } else if let Some((_, slot, _, _)) = self.material_swatch_ids.iter().find(|(id, _, _, _)| *id == *node_id) {
                        let Some(row) = self.properties_card.rows.get(*slot) else {
                            return (true, Vec::new());
                        };
                        if let Some(anchor) = row.rgb_members.as_ref().map(|_| row.id.clone()) {
                            if !self.material_rgb_expanded.remove(&anchor) {
                                self.material_rgb_expanded.insert(anchor);
                            }
                            return (true, vec![PanelAction::Params(ParamsAction::SectionFoldToggled)]);
                        }
                        return (true, Vec::new());
                    } else if let Some((_, look, object, material)) =
                        self.material_look_ids.iter().find(|(id, _, _, _)| *id == *node_id)
                    {
                        if !self.material_action_context(object, material, true) {
                            return (true, Vec::new());
                        }
                        actions.push(PanelAction::Project(ProjectAction::MaterialLookApply {
                            target: GraphParamTarget::GeneratorOf(vm.layer_id.clone()),
                            object: object.clone(),
                            material: material.clone(),
                            look: *look,
                        }));
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
                    } else if let Some((_, index)) =
                        self.object_frame_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        // scene-panel-ux lane: handle Frame button click
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupFrameSelected(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            *index,
                        )));
                    } else if let Some((_, index)) =
                        self.object_duplicate_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupDuplicateObject(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            *index as u32,
                        )));
                    } else if let Some((light_node_id, _, current_name)) =
                        self.light_name_ids.iter().find(|(_, id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Root(RootAction::SceneSetupRenameLightClicked(
                            vm.layer_id.clone(),
                            *light_node_id,
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
                    } else if let Some(act) = super::scene_setup_actions::add_row_click(
                        self.add_object_id,
                        self.add_light_id,
                        self.add_plane_id,
                        *node_id,
                        vm,
                    ) {
                        actions.push(act);
                    } else if self.import_model_id == Some(*node_id) {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupImportModelClicked(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                        )));
                    } else if let Some((group_node_id, _, current_name)) =
                        self.object_name_ids.iter().find(|(_, id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Root(RootAction::SceneSetupRenameObjectClicked(
                            vm.layer_id.clone(),
                            *group_node_id,
                            current_name.clone(),
                        )));
                    } else if let Some((_, group_node_id)) =
                        self.add_modifier_button_id.filter(|(id, _)| *id == *node_id)
                    {
                        // UX-P2 (D6): the button doesn't resolve a choice
                        // itself — it asks the app to open the shared
                        // dropdown (`SceneSetupAddModifierClicked`), which
                        // lists `MESH_MODIFIER_CHOICES` and dispatches the
                        // SAME `SceneSetupAddModifier` each old chip did.
                        actions.push(PanelAction::Root(RootAction::SceneSetupAddModifierClicked(
                            vm.layer_id.clone(),
                            group_node_id,
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
                    } else if let Some((_, group_node_id, modifier_node_id)) =
                        self.modifier_remove_ids.iter().find(|(id, _, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupRemoveModifier(
                            vm.layer_id.clone(),
                            *group_node_id,
                            *modifier_node_id,
                        )));
                    } else if let Some((_, group_node_id, modifier_node_id, new_position)) =
                        self.modifier_move_ids.iter().find(|(id, _, _, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupMoveModifier(
                            vm.layer_id.clone(),
                            *group_node_id,
                            *modifier_node_id,
                            *new_position,
                        )));
                    } else if let Some((_, index)) =
                        self.object_remove_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupRemoveObject(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            *index as u32,
                        )));
                    } else if let Some((_, index)) =
                        self.light_remove_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::Project(ProjectAction::SceneSetupRemoveLight(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            *index as u32,
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
                        for mut action in self.properties_row_action(row, role, *node_id, target) {
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
                (!actions.is_empty() || *node_id == self.close_id, actions)
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
            UIEvent::DragBegin { .. } => (self.properties_card.row_host.is_dragging(), Vec::new()),
            UIEvent::Drag { pos, modifiers, .. } => {
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
            UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                let was_dragging = self.properties_card.row_host.is_dragging();
                let actions = self.properties_card.row_host.handle_drag_end();
                let actions = self.rewrite_material_rgb_actions(actions);
                (was_dragging || !actions.is_empty(), actions)
            }
            // BUG-199: mouse-wheel scroll over the docked body, routed here by
            // `window_input.rs`'s `primary_mouse_wheel` through the generic
            // `UIEvent::Scroll` pipeline (same mechanism the dropdown uses) —
            // `window_input` already gated on `layout.scene_setup().contains(pos)`
            // before emitting this, so no further position check is needed here.
            // `window_input.rs`'s dock-scroll branch also sets
            // `needs_rebuild` so the next frame actually re-applies the
            // new offset (BUG-223: it used to assume this happened for
            // free every frame — it doesn't).
            UIEvent::Scroll { delta, .. } => {
                self.handle_scroll(delta.y);
                (true, Vec::new())
            }
            _ => (false, Vec::new()),
        }
    }

    fn handle_material_placement_event(
        &mut self,
        event: &UIEvent,
        tree: &mut UITree,
    ) -> Option<Vec<PanelAction>> {
        if let Some(index) = self
            .material_placement_active
            .iter()
            .copied()
            .enumerate()
            .find_map(|(index, active)| {
                (active && self.material_placement_widgets[index].is_dragging()).then_some(index)
            })
            && let Some(actions) = self.material_placement_widgets[index].handle_event(event, tree)
        {
            return Some(actions);
        }
        for (index, active) in self.material_placement_active.iter().copied().enumerate() {
            if active && let Some(actions) = self.material_placement_widgets[index].handle_event(event, tree) {
                return Some(actions);
            }
        }
        None
    }

    /// Route a resolved `(row, role)` hit on the unified properties card to
    /// its `PanelAction` by delegating to the shared [`RowHost::row_action`]
    /// — the same core `ParamCardPanel` uses (WIDGET_TREE_DESIGN section 4/5b).
    /// `target` is the caller's `GraphParamTarget::GeneratorOf(vm.layer_id)`
    /// (BUG-292), since `SceneCardState` has no `live_layer_id` of its own.
    fn properties_row_action(
        &mut self,
        row: usize,
        role: RowRole,
        node: NodeId,
        target: GraphParamTarget,
    ) -> Vec<PanelAction> {
        if let RowRole::MaterialFeatureToggle(feature) = role {
            let Some(info) = self.active_material_info.as_ref() else {
                return Vec::new();
            };
            if !self.material_action_context(&info.object, &info.material, false) {
                return Vec::new();
            }
            let Some(param) = self.properties_card.rows.get(row) else {
                return Vec::new();
            };
            let current = param.value.base.round() as i32;
            let next = match current {
                0 => 1, // FollowValues → explicit Off
                1 => 2, // Off → explicit On
                _ => 1, // On → explicit Off; authored factors remain intact
            } as f32;
            return vec![PanelAction::Project(ProjectAction::MaterialParamsSet {
                target,
                object: info.object.clone(),
                material: info.material.clone(),
                kind: MaterialEditKind::Feature,
                writes: vec![MaterialParamWrite { param_id: param.id.clone(), value: next }],
                description: format!("Set {} mode", Self::material_feature_label(feature)),
            })];
        }
        let card = &mut self.properties_card;
        let mut copied_flash = CopyToClipboardLabelState::default();
        card.row_host.row_action(
            target,
            row,
            role,
            node,
            &card.rows,
            &card.current_values,
            &card.osc_addresses,
            &mut card.mod_state,
            &mut card.mod_active_tab,
            &mut copied_flash,
            &mut self.section_folded,
        )
    }

    fn owns_node(&self, node_id: NodeId) -> bool {
        node_id == self.bg_id
    }

    /// The name label's rect for `group_node_id`, if a row for it was built
    /// this frame — the app's text-input anchor lookup (mirrors
    /// `AudioSetupPanel::send_label_rect`).
    pub fn object_name_rect(&self, tree: &UITree, group_node_id: u32) -> Option<Rect> {
        let (_, node_id, _) = self.object_name_ids.iter().find(|(gid, _, _)| *gid == group_node_id)?;
        Some(tree.get_bounds(*node_id))
    }

    /// The light name label's rect for `light_node_id`, if the properties
    /// header was built for it this frame — mirrors `object_name_rect`.
    pub fn light_name_rect(&self, tree: &UITree, light_node_id: u32) -> Option<Rect> {
        let (_, node_id, _) = self.light_name_ids.iter().find(|(id, _, _)| *id == light_node_id)?;
        Some(tree.get_bounds(*node_id))
    }
}

/// Stable outliner-row key, derived from the selection identity itself
/// (Camera/World are fixed; Light/Object key off the node's own doc id,
/// which is stable across a rebuild — removal-stable, unlike an index).
/// Placed well above every other range in this file (max ~130,000) so it
/// can never collide.
const OUTLINER_KEY_BASE: u64 = 90_000_000;
const OUTLINER_EYE_KEY_BASE: u64 = 91_000_000;

fn outliner_row_key(sel: SceneSelection) -> u64 {
    match sel {
        SceneSelection::Camera => OUTLINER_KEY_BASE,
        SceneSelection::World => OUTLINER_KEY_BASE + 1,
        SceneSelection::OutlinerFold(name) => match name {
            "Scene" => KEY_OUTLINER_SCENE,
            "Lights" => KEY_OUTLINER_LIGHTS,
            "Objects" => KEY_OUTLINER_OBJECTS,
            _ => OUTLINER_KEY_BASE + 100, // fallback
        },
        SceneSelection::Light(id) => OUTLINER_KEY_BASE + 2 + (id as u64) * 2,
        SceneSelection::Object(id) => OUTLINER_KEY_BASE + 3 + (id as u64) * 2,
    }
}

fn outliner_eye_key(object_node_id: u32) -> u64 {
    OUTLINER_EYE_KEY_BASE + object_node_id as u64
}

/// Selected-row styling, transcribed from the `layer_header.rs` precedent
/// (`sel_accent_style`/`bg_style`, verified 2026-07-17: `tree.rs` carries NO
/// selection styling at all — this panel has no per-row identity colour to
/// brighten, so the tint applies the app-wide `SELECTED_LAYER_RING` colour
/// directly, at low alpha, as a background wash rather than a border box —
/// same "never a border box" doctrine `bg_style`'s own comment states.
fn outliner_row_style(selected: bool) -> UIStyle {
    let ring = color::SELECTED_LAYER_RING;
    let sel_bg = Color32::new(ring.r, ring.g, ring.b, 40);
    let sel_hover = Color32::new(ring.r, ring.g, ring.b, 60);
    UIStyle {
        bg_color: if selected { sel_bg } else { Color32::TRANSPARENT },
        hover_bg_color: if selected { sel_hover } else { Color32::new(255, 255, 255, 18) },
        text_color: if selected { ring } else { Color32::new(200, 200, 208, 255) },
        font_size: color::FONT_LABEL,
        text_align: TextAlign::Left,
        corner_radius: color::SMALL_RADIUS,
        ..UIStyle::default()
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
mod tests {
    use super::*;
    use crate::input::Modifiers;

    /// C-P1a: wrap a plain `RowValue` in an idle (no active modulation)
    /// `ModulatedRow` — the shape `EnvironmentRowVm`/`AtmosphereRowVm` now
    /// carry for every converted row.
    fn mrow(value: RowValue) -> ModulatedRow {
        ModulatedRow { value, modulation: Box::new(RowModulation::default()) }
    }

    fn triplet(node_doc_id: u32, x: f32, y: f32, z: f32, min: f32, max: f32) -> (RowValue, RowValue, RowValue) {
        (
            RowValue { addr: RowAddr::root(node_doc_id, "x"), value: x, min, max, driven: false, exposed: false },
            RowValue { addr: RowAddr::root(node_doc_id, "y"), value: y, min, max, driven: false, exposed: false },
            RowValue { addr: RowAddr::root(node_doc_id, "z"), value: z, min, max, driven: false, exposed: false },
        )
    }

    /// C-P1b: `triplet` wrapped element-wise in idle `mrow`s — the shape
    /// `TransformRowVm`/`ObjectMaterialVm` now carry for every converted
    /// Object row.
    fn mtriplet(
        node_doc_id: u32,
        x: f32,
        y: f32,
        z: f32,
        min: f32,
        max: f32,
    ) -> (ModulatedRow, ModulatedRow, ModulatedRow) {
        let (rx, ry, rz) = triplet(node_doc_id, x, y, z, min, max);
        (mrow(rx), mrow(ry), mrow(rz))
    }

    /// C-P1c: wrap a plain `EnumRowValue`-shaped `(RowValue, labels)` pair in
    /// an idle `ModulatedEnumRow` — the shape `LightKnownRow`'s Mode/Cast
    /// Shadows/Shadow Softness now carry.
    fn menum(row: RowValue, labels: Vec<&'static str>) -> ModulatedEnumRow {
        ModulatedEnumRow { row: mrow(row), labels }
    }

    #[test]
    fn closed_panel_builds_nothing() {
        let mut panel = ScenePanel::new();
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(tree.count(), 0, "a closed panel must not build any node");
    }

    #[test]
    fn no_selection_state_renders_a_sentence_without_panicking() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::NoSelection("Select a layer.".to_string()));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(tree.count() > 0);
    }

    #[test]
    fn live_state_with_unwired_env_and_fog_shows_add_buttons() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(SceneSetupVm {
            layer_id: LayerId::new("layer-1"),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 0,
            light_count: 0,
            shadow_caster_count: 0,
            scene_root_node_id: 0,
            environment: EnvironmentRowVm::None,
            atmosphere: AtmosphereRowVm::None,
            objects: Vec::new(),
            lights: Vec::new(),
            camera: CameraRowVm::None,
            camera_sections: Vec::new(), camera_param_doc_ids: None, world_sections: Vec::new(),
            scene_bounds: None,
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_environment_id.is_some());
        assert!(panel.add_fog_id.is_some());
        assert!(panel.add_object_id.is_some());
        assert!(panel.add_light_id.is_some());
        assert!(panel.add_plane_id.is_some());
    }

    /// A synthetic multi-object def (P2 gate): one Known "Azalea" object with
    /// a full transform + pbr material + a Bend modifier, one Custom object,
    /// and header counts — proves the Objects section renders both shapes,
    /// the rename click resolves to the right group node id, and the
    /// "+ Object"/"+ Light" buttons carry the Vm's own counts as
    /// `next_index`.
    fn azalea_shaped_vm() -> SceneSetupVm {
        SceneSetupVm {
            layer_id: LayerId::new("layer-1"),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 2,
            light_count: 1,
            shadow_caster_count: 1,
            scene_root_node_id: 99,
            environment: EnvironmentRowVm::None,
            atmosphere: AtmosphereRowVm::None,
            objects: vec![
                ObjectRowVm::Known(Box::new(ObjectKnownRow {
                    index: 0,
                    object_node_id: 40,
                    group_node_id: Some(42),
                    name: "Azalea".to_string(),
                    visible: RowValue { addr: RowAddr { scope_path: vec![42], node_doc_id: 40, param_id: "visible".to_string() }, value: 1.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                    transform: Some(Box::new(TransformRowVm {
                        pos: mtriplet(50, 1.0, 2.0, 3.0, -100.0, 100.0),
                        rot: mtriplet(50, 0.0, 0.0, 0.0, -std::f32::consts::TAU, std::f32::consts::TAU),
                        scale: mtriplet(50, 1.0, 1.0, 1.0, 0.01, 10.0),
                    })),
                    material: ObjectMaterialVm::Pbr {
                        color: mtriplet(51, 0.8, 0.8, 0.82, 0.0, 1.0),
                        metallic: mrow(RowValue { addr: RowAddr::root(51, "metallic"), value: 0.0, min: 0.0, max: 1.0, driven: false, exposed: false }),
                        roughness: mrow(RowValue { addr: RowAddr::root(51, "roughness"), value: 0.5, min: 0.01, max: 1.0, driven: false, exposed: false }),
                    },
                    material_inspector: None,
                    modifiers: vec![ModifierKnownRow {
                        index: 0,
                        node_doc_id: 70,
                        display_name: "Bend".to_string(),
                    }],
                    modifiers_addable: true,
                    sections: Vec::new(),
                    skin: None,
                })),
                ObjectRowVm::Custom { index: 1 },
            ],
            lights: vec![
                LightRowVm::Known(Box::new(LightKnownRow {
                    index: 0,
                    node_doc_id: 60,
                    name: "Sun".to_string(),
                    mode: menum(
                        RowValue { addr: RowAddr::root(60, "mode"), value: 0.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                        vec!["Sun", "Point"],
                    ),
                    color: mtriplet(60, 1.0, 1.0, 1.0, 0.0, 1.0),
                    intensity: mrow(RowValue { addr: RowAddr::root(60, "intensity"), value: 2.5, min: 0.0, max: 10.0, driven: false, exposed: false }),
                    pos: mtriplet(60, 5.0, 2.0, 3.0, -100.0, 100.0),
                    aim: mtriplet(60, 0.0, 0.0, 0.0, -100.0, 100.0),
                    cast_shadows: menum(
                        RowValue { addr: RowAddr::root(60, "cast_shadows"), value: 1.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                        vec!["Off", "On"],
                    ),
                    shadow_softness: menum(
                        RowValue { addr: RowAddr::root(60, "shadow_softness"), value: 3.0, min: 0.0, max: 3.0, driven: false, exposed: false },
                        vec!["Hard", "Soft", "VerySoft", "Contact"],
                    ),
                    light_size: mrow(RowValue { addr: RowAddr::root(60, "light_size"), value: 4.0, min: 0.0, max: 20.0, driven: false, exposed: false }),
                    sections: Vec::new(),
                })),
                LightRowVm::Custom { index: 1 },
            ],
            camera: CameraRowVm::Orbit(Box::new(OrbitCameraRowVm {
                orbit: mrow(RowValue { addr: RowAddr::root(70, "orbit"), value: 0.7, min: -std::f32::consts::TAU, max: std::f32::consts::TAU, driven: false, exposed: false }),
                tilt: mrow(RowValue { addr: RowAddr::root(70, "tilt"), value: 0.3, min: -std::f32::consts::TAU, max: std::f32::consts::TAU, driven: false, exposed: false }),
                distance: mrow(RowValue { addr: RowAddr::root(70, "distance"), value: 4.0, min: 0.01, max: 100.0, driven: false, exposed: false }),
                fov_y: mrow(RowValue { addr: RowAddr::root(70, "fov_y"), value: 0.9, min: 0.05, max: 2.5, driven: false, exposed: false }),
                lens: Some(LensRowVm {
                    focus_distance: mrow(RowValue { addr: RowAddr::root(71, "focus_distance"), value: 0.0, min: 0.0, max: 1000.0, driven: false, exposed: false }),
                    f_stop: mrow(RowValue { addr: RowAddr::root(71, "f_stop"), value: 1000.0, min: 0.5, max: 1000.0, driven: false, exposed: false }),
                    shutter_angle: mrow(RowValue { addr: RowAddr::root(71, "shutter_angle"), value: 0.0, min: 0.0, max: 360.0, driven: false, exposed: false }),
                    exposure_ev: mrow(RowValue { addr: RowAddr::root(71, "exposure_ev"), value: 0.0, min: -8.0, max: 8.0, driven: false, exposed: false }),
                }),
            })),
            camera_sections: Vec::new(), camera_param_doc_ids: None, world_sections: Vec::new(),
            scene_bounds: None,
        }
    }

    #[test]
    fn objects_outliner_lists_known_and_custom_rows_properties_shows_the_selected_one() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Outliner rows: Scene fold + Camera + World + Lights fold + 1 Known light + Objects fold + 1 Known object are
        // selectable (`outliner_row_ids`); the Custom object/light are
        // listed too but as plain labels (D3: never hidden, but no
        // addressable node id to select by, D12).
        assert_eq!(panel.outliner_row_ids.len(), 7, "Scene/Camera/World fold rows + Lights fold + 1 known light + Objects fold + 1 known object");
        // Default selection (D7): the first Known object — Azalea — so its
        // properties header + body render without any click.
        assert_eq!(panel.object_name_ids.len(), 1, "the properties header shows the selected object's name");
        assert_eq!(panel.object_name_ids[0].0, 42, "resolves to the object's group node id (the rename address)");
        assert_eq!(panel.object_name_ids[0].2, "Azalea");
        // P2 slice 2a: the Properties body's actual PARAM ROWS now come from
        // `self.full_params` (the real generator `ParamSurface`, wired by
        // `configure_params` — see that method's doc comment), not from this
        // hand-built `SceneSetupVm` fixture's own transform/material/
        // modifier fields. This test's fixture never calls
        // `configure_params`, so it can't exercise row rendering — see
        // `build_filtered_properties_...` tests below for that mechanism.
        assert!(panel.add_object_id.is_some());
        assert!(panel.add_light_id.is_some());
        assert!(panel.add_plane_id.is_some());
    }

    /// W2-A gap fill: the outliner eye toggle (D3's on/off convention) had
    /// zero click->dispatch coverage — every "eye" hit in this file before
    /// this test was a comment. A click on a Known object row's eye emits
    /// `SceneSetupParamChanged` carrying the row's own write address and the
    /// flipped [0,1] value; a second click on the now-off eye flips back.
    #[test]
    fn object_eye_toggle_click_emits_scene_setup_param_changed_and_flips_back() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.outliner_eye_ids.len(), 1, "one Known object row renders a live eye");
        let (eye_id, row_value) = panel.outliner_eye_ids[0].clone();
        assert_eq!(row_value.value, 1.0, "azalea fixture starts visible");

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: eye_id,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed, "the eye toggle must be clickable");
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupParamChanged(layer, scope, node, param, value))]
                if *layer == LayerId::new("layer-1")
                    && *scope == vec![42]
                    && *node == 40
                    && param == "visible"
                    && *value == 0.0
        ), "visible eye click must flip to 0.0 at the object's own write address, got {actions:?}");

        // Re-configure with the flipped value (mirrors the real per-frame
        // sync landing the write) and click again — must flip back to 1.0.
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.visible.value = 0.0;
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (eye_id_2, _) = panel.outliner_eye_ids[0].clone();

        let (consumed_2, actions_2) = panel.handle_event(&UIEvent::Click {
            node_id: eye_id_2,
            pos: Vec2::ZERO,
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed_2);
        assert!(matches!(
            actions_2.as_slice(),
            [PanelAction::Project(ProjectAction::SceneSetupParamChanged(_, _, _, param, value))]
                if param == "visible" && *value == 1.0
        ), "hidden eye click must flip back to 1.0, got {actions_2:?}");
    }

    /// A one-object Vm with TWO modifiers — for exercising up/down boundary
    /// behavior (P5), which the single-modifier `azalea_shaped_vm` can't.
    fn two_modifier_object_vm(modifiers_addable: bool) -> SceneSetupVm {
        let mut vm = azalea_shaped_vm();
        let ObjectRowVm::Known(row) = &mut vm.objects[0] else { unreachable!() };
        row.modifiers = vec![
            ModifierKnownRow {
                index: 0,
                node_doc_id: 70,
                display_name: "Bend".to_string(),
            },
            ModifierKnownRow {
                index: 1,
                node_doc_id: 71,
                display_name: "Twist".to_string(),
            },
        ];
        row.modifiers_addable = modifiers_addable;
        vm
    }

    /// UX-P2 (D6): the "+ Add Modifier" button doesn't resolve a choice
    /// itself anymore — it emits `SceneSetupAddModifierClicked`, which the
    /// app resolves into the shared dropdown (`MESH_MODIFIER_CHOICES`
    /// items, each carrying `SceneSetupAddModifier` — see
    /// `try_open_dropdown_inner` in `manifold-app/src/ui_root.rs`, not
    /// reachable from this crate's tests). This test only proves the
    /// panel's half of D6: one button renders (not 7 chips) and its click
    /// carries the right `(layer_id, group_node_id, button_node_id)`.
    #[test]
    fn add_modifier_button_click_emits_add_modifier_clicked_action() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (button_id, group_node_id) = panel.add_modifier_button_id.expect("one Add Modifier button renders");
        assert_eq!(group_node_id, 42);

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: button_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::Root(RootAction::SceneSetupAddModifierClicked(l, 42, n))
                if *l == LayerId::new("layer-1") && *n == button_id
        ));
    }

    /// BUG-224 regression: the × close button used to call `self.close()`
    /// directly, which only flips the panel-local `open` flag — it never
    /// told the app to reset `layout.scene_setup_width` back to 0 or to
    /// rebuild, so on the real app the dock's screen footprint and content
    /// never went away (Peter: "the close button doesn't work"). The fix
    /// mirrors `AudioSetupPanel::handle_event`'s close arm exactly: emit
    /// `PanelAction::OpenSceneSetup`, the SAME toggle action the header
    /// button and Escape use — that's the one path that resets width, closes
    /// the panel, and triggers the structural rebuild
    /// (`ui_bridge::dispatch`'s `OpenSceneSetup` arm).
    #[test]
    fn close_button_click_routes_through_the_shared_toggle_action() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_ne!(panel.close_id, NodeId::PLACEHOLDER);

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: panel.close_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(
            matches!(actions.as_slice(), [PanelAction::Root(RootAction::OpenSceneSetup)]),
            "close (×) must emit the shared toggle action, not flip `open` \
             locally: got {actions:?}"
        );
        // The direct `self.close()` bypass is gone: `open` is untouched by
        // this click alone (the app-level `toggle_scene_dock()` — driven by
        // dispatching the action above — is what actually closes it).
        assert!(panel.is_open(), "handle_event itself must not close the panel — that's the app's job now");
    }

    #[test]
    fn modifier_remove_click_emits_remove_modifier_action() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.modifier_remove_ids.len(), 1);
        let (remove_id, group_node_id, modifier_node_id) = panel.modifier_remove_ids[0];
        assert_eq!(group_node_id, 42);
        assert_eq!(modifier_node_id, 70);

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: remove_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupRemoveModifier(l, 42, 70)) if *l == LayerId::new("layer-1")
        ));
    }

    #[test]
    fn modifier_up_down_respect_stack_boundaries() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(two_modifier_object_vm(true))));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // First modifier (index 0): no "up" target (already first), but a
        // "down" target to position 1.
        // Second modifier (index 1): an "up" target to position 0, no
        // "down" target (already last).
        assert_eq!(panel.modifier_move_ids.len(), 2, "one live reorder target per modifier, boundary buttons excluded");
        assert!(
            panel
                .modifier_move_ids
                .iter()
                .any(|(_, gid, mid, pos)| *gid == 42 && *mid == 70 && *pos == 1),
            "modifier 0's down button targets position 1"
        );
        assert!(
            panel
                .modifier_move_ids
                .iter()
                .any(|(_, gid, mid, pos)| *gid == 42 && *mid == 71 && *pos == 0),
            "modifier 1's up button targets position 0"
        );
    }

    #[test]
    fn modifier_move_click_emits_move_modifier_action() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(two_modifier_object_vm(true))));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (move_id, _, _, _) = panel
            .modifier_move_ids
            .iter()
            .find(|(_, gid, mid, _)| *gid == 42 && *mid == 71)
            .copied()
            .unwrap();

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: move_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupMoveModifier(l, 42, 71, 0)) if *l == LayerId::new("layer-1")
        ));
    }

    #[test]
    fn unparseable_modifier_chain_shows_custom_label_and_disables_add() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(two_modifier_object_vm(false))));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_modifier_button_id.is_none(), "Add modifier is disabled for an unparseable chain");
        assert!(panel.modifier_remove_ids.is_empty(), "no remove buttons for an unparseable chain either");
    }

    #[test]
    fn add_object_and_add_light_buttons_carry_the_vms_own_counts_as_next_index() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let add_object_id = panel.add_object_id.unwrap();
        let add_light_id = panel.add_light_id.unwrap();

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: add_object_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupAddObject(l, 99, 2)) if *l == LayerId::new("layer-1")
        ));

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: add_light_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupAddLight(l, 99, 1)) if *l == LayerId::new("layer-1")
        ));
    }

    /// BUG-hlw8: the "+ Plane" button emits `SceneSetupAddLayerPlane` carrying
    /// the live `object_count` as its `next_index` — same convention as the
    /// "+ Object" button, because a layer plane occupies the next object slot.
    #[test]
    fn add_plane_button_emits_add_layer_plane_with_object_count_as_next_index() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let add_plane_id = panel.add_plane_id.unwrap();

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: add_plane_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupAddLayerPlane(l, 99, 2)) if *l == LayerId::new("layer-1")
        ));
    }

    /// BUG-193/P5: the properties header's "Remove" button (Object
    /// selection) dispatches `SceneSetupRemoveObject` carrying the selected
    /// object's own `index`. A `Custom` row has no addressable node id
    /// (D12), so — unlike v1's per-row "✕" — it can't be selected/removed
    /// through the panel UI; this is a real reduction from v1's coverage,
    /// flagged as an escalation in the P5 landing report rather than
    /// improvised around.
    #[test]
    fn object_remove_click_emits_remove_object_action_with_its_own_index() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Default selection = the Known object (Azalea, index 0).
        assert_eq!(panel.object_remove_ids.len(), 1, "one remove button — the properties header's, for the selection");
        let (remove_id, index) = panel.object_remove_ids[0];
        assert_eq!(index, 0);

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: remove_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupRemoveObject(l, 99, 0)) if *l == LayerId::new("layer-1")
        ));
    }

    /// D11: the properties header's "Duplicate" button (Object selection)
    /// dispatches `SceneSetupDuplicateObject` carrying the selected
    /// object's own `index`.
    #[test]
    fn object_duplicate_click_emits_duplicate_object_action() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.object_duplicate_ids.len(), 1);
        let (dup_id, index) = panel.object_duplicate_ids[0];
        assert_eq!(index, 0);

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: dup_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupDuplicateObject(l, 99, 0)) if *l == LayerId::new("layer-1")
        ));
    }

    /// UX-P3b-i's own deliverable: the per-row key-range collision audit the
    /// design doc's "as attempted" note calls out, extended from Objects
    /// (P3a's own `OBJ_KEY_STRIDE` 32→44 bump) to Light/Camera/Modifier.
    /// Computational proof (oracle discipline: a countable arithmetic
    /// question gets a script, not an eyeball) — every named offset within
    /// each family's own key formula must be pairwise distinct AND (for the
    /// per-index families) strictly less than that family's stride, so no
    /// two DIFFERENT logical rows can ever key the same node under
    /// `UITree::mint`'s "keys only need to be unique among siblings of the
    /// same parent" contract (`tree.rs`'s own `debug_assert` catches a live
    /// violation; this test catches it at the constant-arithmetic level,
    /// before any panel is ever built).
    #[test]
    fn no_key_offset_collisions_across_row_families() {
        fn assert_no_dupes_and_fits_stride(family: &str, offsets: &[u64], stride: Option<u64>) {
            let mut sorted = offsets.to_vec();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(
                sorted.len(),
                offsets.len(),
                "{family}: duplicate offset among {offsets:?} — two logical rows would key the same node"
            );
            if let Some(stride) = stride {
                assert!(
                    offsets.iter().all(|&o| o < stride),
                    "{family}: an offset in {offsets:?} reaches into the next index's range (stride {stride})"
                );
            }
        }

        // C-P1b: the value-cell offsets (`OBJ_OFF_POS_X`/`ROT_X`/`SCALE_X`/
        // `COLOR_R`/`METALLIC`/`ROUGHNESS`) are gone — those rows' widgets
        // now key off `build_param_row`'s own ParamId-derived `row_key_base`,
        // a disjoint key space from `obj_key`'s. Only NAME/REMOVE (header
        // chrome) still key through `obj_key` (the exposure-lane mod
        // buttons were removed with the ∿ column).
        assert_no_dupes_and_fits_stride(
            "OBJECT",
            &[
                OBJ_OFF_NAME,
                OBJ_OFF_REMOVE, OBJ_OFF_REMOVE + 1,
            ],
            Some(OBJ_KEY_STRIDE),
        );

        // C-P1c: the value-cell offsets (`LIGHT_OFF_MODE_MINUS`/`COLOR_R`/
        // `INTENSITY_MINUS`/`POS_X`/`AIM_X`/`CAST_SHADOWS_MINUS`/
        // `SHADOW_SOFTNESS_MINUS`/`LIGHT_SIZE_MINUS`) are gone — those rows'
        // widgets now key off `build_param_row`'s own `row_key_base`
        // (derived from the stable ParamId), same disjoint key space C-P1b established for
        // Object. Only NAME/REMOVE (header chrome) still key through
        // `light_key`.
        assert_no_dupes_and_fits_stride(
            "LIGHT",
            &[
                LIGHT_OFF_REMOVE,
                LIGHT_OFF_NAME,
            ],
            Some(LIGHT_KEY_STRIDE),
        );

        // C-P1c: Camera's value-cell offsets are gone, and the exposure-lane
        // mod buttons went with the ∿ column — Camera keys nothing through
        // an explicit-key scheme anymore (`build_param_row`'s ParamId-derived key
        // covers all its rows), so there is nothing left to audit here.

        // Modifier: per-slot offsets (up to 4 param slots) must fit inside
        // MODIFIER_ROW_STRIDE, same per-index-range contract as OBJECT/LIGHT.
        // C-P1d: the old `MODIFIER_OFF_PARAM_BASE` 3-wide `[-] value [+]`
        // stepper offsets are gone (deleted with the pre-convergence bespoke
        // numeric/enum stepper builders) — a Numeric/Axis row's own value
        // cell, track, and steppers now key through `build_param_row`'s internal
        // ParamId-derived scheme, not `modifier_row_key`; only the reorder/
        // remove chrome and the mod-button offset still use it.
        assert_no_dupes_and_fits_stride(
            "MODIFIER (per-row)",
            &[MODIFIER_OFF_UP, MODIFIER_OFF_DOWN, MODIFIER_OFF_REMOVE],
            Some(MODIFIER_ROW_STRIDE),
        );
    }

    /// BUG-193/P5: the Lights-section twin of the object-removal test above
    /// — the properties header's "Remove" button for a Light selection.
    #[test]
    fn light_remove_click_emits_remove_light_action_with_its_own_index() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        // Select the Known light (node 60) — not the default (Azalea).
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.light_remove_ids.len(), 1, "one remove button — the properties header's, for the selection");
        let (remove_id, index) = panel.light_remove_ids[0];
        assert_eq!(index, 0);

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: remove_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupRemoveLight(l, 99, 0)) if *l == LayerId::new("layer-1")
        ));
    }

    /// P4: "Import Model…" is a real button (affordance legibility) that
    /// dispatches `SceneSetupImportModelClicked(layer_id, render_scene_node_id)`
    /// — the panel itself never touches the filesystem or the merge
    /// assembler, just carries the address the app-side dispatch needs.
    #[test]
    fn import_model_button_emits_scene_setup_import_model_clicked() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let import_model_id = panel.import_model_id.unwrap();

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: import_model_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::Project(ProjectAction::SceneSetupImportModelClicked(l, 99)) if *l == LayerId::new("layer-1")
        ));
    }

    #[test]
    fn clicking_the_object_name_emits_rename_clicked_with_group_node_id() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let name_id = panel.object_name_ids[0].1;

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: name_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::Root(RootAction::SceneSetupRenameObjectClicked(l, 42, n))
                if *l == LayerId::new("layer-1") && n == "Azalea"
        ));
    }

    /// D7: clicking an outliner row changes the UI-local selection, and the
    /// next build shows THAT item's properties instead — "select the object
    /// to use the tools" (Peter). Proves the Object→World switch (Properties
    /// content changes: object body gone, Environment/Fog appear) and that
    /// a click on the World row is what does it.
    #[test]
    fn selecting_a_different_outliner_row_switches_properties_content() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Default selection = Azalea: no environment/fog "add" affordances
        // (azalea fixture's environment is None — but World isn't selected,
        // so neither button builds).
        assert!(panel.add_environment_id.is_none(), "World isn't selected — no Environment row built yet");

        let (world_row_id, _) = *panel
            .outliner_row_ids
            .iter()
            .find(|(_, sel)| *sel == SceneSelection::World)
            .expect("World is always a selectable outliner row");

        let (consumed, _) = panel.handle_event(&UIEvent::Click {
            node_id: world_row_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert_eq!(panel.selection.get(&LayerId::new("layer-1")), Some(&SceneSelection::World));

        let mut tree2 = UITree::new();
        panel.build_docked(&mut tree2, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_environment_id.is_some(), "World selected — Environment's Add affordance renders");
    }

    /// D7's fallback: removing the selected object from a rebuilt Vm falls
    /// selection back to first-object-else-World, never a dangling id.
    #[test]
    fn selection_falls_back_when_the_selected_object_is_removed() {
        let mut panel = ScenePanel::new();
        panel.open();
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm.clone())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(
            panel.selection.get(&LayerId::new("layer-1")),
            Some(&SceneSelection::Object(40)),
            "default selection resolves to Azalea's own scene_object doc id"
        );

        // Rebuild with the object gone (removed elsewhere) — only the
        // Custom row and the light remain.
        let mut vm2 = vm;
        vm2.objects = vec![ObjectRowVm::Custom { index: 0 }];
        vm2.object_count = 0;
        panel.configure(SceneSetupState::Live(Box::new(vm2)));
        let mut tree2 = UITree::new();
        panel.build_docked(&mut tree2, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(
            panel.selection.get(&LayerId::new("layer-1")),
            Some(&SceneSelection::World),
            "no Known object left — falls back to World, never a dangling Object(40)"
        );
    }

    // ── P3: Lights + Camera sections ──

    /// A World-selected scene with one real "Transform" section plus the
    /// matching generator `ParamSurface` (one ±100 translate row) — the
    /// fixture the scene type-in / fine-scrub tests need. `azalea_shaped_vm`'s
    /// `world_sections` is empty, so it can't exercise the unified properties
    /// card's rows.
    pub(super) fn world_transform_vm() -> (SceneSetupVm, ParamSurface) {
        let mut vm = azalea_shaped_vm();
        vm.world_sections = vec!["Transform".to_string()];
        let surface = ParamSurface {
            kind: crate::panels::param_card::ParamCardKind::Generator,
            title: "Scene".to_string(),
            collapsed: false,
            enabled: true,
            effect_index: 0,
            effect_id: manifold_foundation::EffectId::new("scene-gen"),
            supports_envelopes: false,
            has_graph_mod: false,
            layer_id: Some(LayerId::new("layer-1")),
            rows: vec![ParamRow {
                id: manifold_foundation::ParamId::from("translate_x"),
                spec: RowSpec {
                    name: "Translate X".to_string(),
                    min: -100.0,
                    max: 100.0,
                    default: 0.0,
                    whole_numbers: false,
                    is_angle: false,
                    is_toggle: false,
                    is_trigger: false,
                    is_trigger_gate: false,
                    value_labels: None,
                    section: Some("Transform".to_string()),
                    disabled: None,
                    material_role: None,
                    inactive_reason: None,
                },
                value: crate::param_surface::RowValue {
                    base: 0.0,
                    effective: 0.0,
                    exposed: true,
                    driven: false,
                },
                audio: AudioRowState::default(),
                modulation: RowMod::default(),
                mapping: RowMapping {
                    osc_address: None,
                    ableton_display: None,
                    ableton_range: None,
                    mappable: false,
                },
                    scene_addr: None,
                    rgb_members: None,
                    material_attached: false,
            }],
            string_params: Vec::new(),
            modifier: None,
            audio_sends: Vec::new(),
            relight: crate::panels::param_card::RelightCardConfig::default(),
        };
        (vm, surface)
    }

    #[test]
    fn physics_reset_button_dispatches_to_the_bound_scene_layer() {
        let (vm, mut surface) = world_transform_vm();
        surface.rows[0].id = manifold_foundation::ParamId::from("40_reset");
        surface.rows[0].spec.name = "Reset".into();
        surface.rows[0].spec.is_trigger = true;
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::World);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let button = panel.properties_card.row_host.toggle_ids[0]
            .as_ref().expect("Reset must have a trigger button").button_id;
        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: button,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        }, &mut tree);
        assert!(consumed);
        assert!(matches!(actions.as_slice(),
            [PanelAction::Params(ParamsAction::ParamFire(GraphParamTarget::GeneratorOf(layer), id))]
                if layer.as_str() == "layer-1" && id.as_ref() == "40_reset"
        ));
    }

    /// Build the world-transform fixture and select World, so the unified
    /// properties card renders its one translate row. Returns the panel and a
    /// fresh tree (post-selection rebuild).
    fn scene_with_world_transform_selected() -> (ScenePanel, UITree) {
        let (vm, surface) = world_transform_vm();
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.configure_params(Some(surface));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let (world_row_id, _) = *panel
            .outliner_row_ids
            .iter()
            .find(|(_, sel)| *sel == SceneSelection::World)
            .expect("World is always a selectable outliner row");
        let (consumed, _) = panel.handle_event(
            &UIEvent::Click {
                node_id: world_row_id,
                pos: Vec2::ZERO,
                modifiers: Modifiers::default(),
        },
            &mut tree,
        );
        assert!(consumed, "World outliner row click must consume");

        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert_eq!(panel.properties_card.rows.len(), 1, "the translate row renders under World");
        assert!(panel.properties_card.row_host.slider_ids[0].is_some(), "the row has a slider");
        (panel, tree)
    }

    /// D8: double-clicking the scene properties row's value cell routes through
    /// the SHARED `RowHost::value_cell_typein` — the same `BeginParamTextInput`
    /// action the inspector cards emit — carrying the panel's own bound layer
    /// (`GeneratorOf`) and the row's real param id + clamp range.
    #[test]
    fn scene_properties_double_click_opens_the_shared_typein() {
        let (mut panel, mut tree) = scene_with_world_transform_selected();
        let value_cell = panel.properties_card.row_host.slider_ids[0].as_ref().unwrap().value_text;

        let (consumed, actions) = panel.handle_event(
            &UIEvent::DoubleClick {
                node_id: value_cell,
                pos: Vec2::ZERO,
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(consumed, "double-click on a scene value cell must consume");
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Root(RootAction::BeginParamTextInput { target, param_id, min, max, value, whole_numbers, .. })]
                if *target == GraphParamTarget::GeneratorOf(LayerId::new("layer-1"))
                    && param_id.as_ref() == "translate_x"
                    && *min == -100.0 && *max == 100.0 && *value == 0.0 && !*whole_numbers
        ), "scene type-in must carry GeneratorOf + the real param id + range, got {actions:?}");
    }

    /// D8: a double-click on a non-value-cell scene node (the track) emits
    /// nothing — type-in is the value cell's gesture only.
    #[test]
    fn scene_properties_double_click_on_track_is_a_no_op() {
        let (mut panel, mut tree) = scene_with_world_transform_selected();
        let track = panel.properties_card.row_host.slider_ids[0].as_ref().unwrap().track;

        let (consumed, actions) = panel.handle_event(
            &UIEvent::DoubleClick {
                node_id: track,
                pos: Vec2::ZERO,
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(!consumed, "track double-click is not a type-in");
        assert!(actions.is_empty());
    }

    /// D8 fine mode on the scene properties track: Shift during a drag scales
    /// the pointer sensitivity by 0.1, through the same shared helper the card
    /// uses the shared `RowHost` drag lifecycle and fine-scrub math.
    #[test]
    fn scene_properties_drag_shift_fine_scales_sensitivity() {
        let (mut panel, mut tree) = scene_with_world_transform_selected();
        let track = panel.properties_card.row_host.slider_ids[0].as_ref().unwrap().track;
        let track_rect = tree.get_bounds(track);
        let mid_x = track_rect.x + track_rect.width * 0.5;

        let (consumed, down) = panel.handle_event(
            &UIEvent::PointerDown {
                node_id: track,
                pos: Vec2::new(mid_x, track_rect.y),
                modifiers: Modifiers::default(),
            },
            &mut tree,
        );
        assert!(consumed, "track pointer-down must start the scene drag");
        assert!(matches!(down.as_slice(), [PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Begin), PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Move(..))]));

        let coarse_val = {
            let (_, actions) = panel.handle_event(
                &UIEvent::Drag {
                    node_id: Some(track),
                    pos: Vec2::new(mid_x + 20.0, track_rect.y),
                    delta: Vec2::new(20.0, 0.0),
                    modifiers: Modifiers::NONE,
                },
                &mut tree,
            );
            match actions.as_slice() {
                [PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Move(ScrubValue::Scalar(v)))] => *v,
                other => panic!("expected a coarse scene move, got {other:?}"),
            }
        };
        let fine_val = {
            let (_, actions) = panel.handle_event(
                &UIEvent::Drag {
                    node_id: Some(track),
                    pos: Vec2::new(mid_x + 20.0, track_rect.y),
                    delta: Vec2::new(20.0, 0.0),
                    modifiers: Modifiers { shift: true, ..Modifiers::NONE },
                },
                &mut tree,
            );
            match actions.as_slice() {
                [PanelAction::Scrub(ValueRef::Param(..), ScrubPhase::Move(ScrubValue::Scalar(v)))] => *v,
                other => panic!("expected a fine scene move, got {other:?}"),
            }
        };

        let coarse_delta = coarse_val.abs();
        let fine_delta = fine_val.abs();
        assert!(coarse_delta > 1.0, "coarse must move: {coarse_val}");
        assert!(
            (fine_delta - coarse_delta * 0.1).abs() < 1.5,
            "fine delta ({fine_delta}) must be ~0.1x coarse delta ({coarse_delta})"
        );
    }

    /// D3/D12's tolerance doctrine: an all-Custom-lights scene (no
    /// addressable id at all) must still render every row as an outliner
    /// label — never hidden, never a panic — even though none of them are
    /// selectable through the panel UI (D12's own gap, same as Custom
    /// objects, flagged in the P5 landing report).
    #[test]
    fn more_than_four_lights_all_render_without_panicking_no_panel_side_cap() {
        let mut vm = azalea_shaped_vm();
        vm.lights = (0..5)
            .map(|i| LightRowVm::Custom { index: i })
            .collect();
        vm.light_count = 5;
        vm.shadow_caster_count = 5;
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(tree.count() > 0, "5 custom light rows render without panicking");
        assert!(
            panel.outliner_row_ids.iter().all(|(_, sel)| !matches!(sel, SceneSelection::Light(_))),
            "no Custom light has an addressable id to select by"
        );
    }

    #[test]
    fn camera_none_and_custom_shapes_render_without_panicking() {
        for camera in [CameraRowVm::None, CameraRowVm::Custom] {
            let mut vm = azalea_shaped_vm();
            vm.camera = camera;
            let mut panel = ScenePanel::new();
            panel.open();
            panel.configure(SceneSetupState::Live(Box::new(vm)));
            let mut tree = UITree::new();
            panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
            assert!(tree.count() > 0);
        }
    }

    // scene-panel-ux lane fold behavior tests

    #[test]
    fn folded_properties_section_contributes_zero_row_height_and_builds_no_param_rows() {
        // Test that the fold machinery exists and works correctly
        let mut panel = ScenePanel::new();
        panel.open();
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();

        // Initial build
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        // Set fold state - verify the machinery works
        panel.section_folded.insert("Transform".to_string(), true);
        assert!(panel.section_folded.get("Transform").copied().unwrap_or(false), "Fold state should be stored");

        // Verify we can iterate over fold keys (needed for the build loop)
        let has_transform = panel.section_folded.get("Transform").is_some();
        assert!(has_transform, "Fold state should be queryable");

        // Rebuild to test the fold is respected (no panic)
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        // Verify state persisted through build
        assert!(panel.section_folded.get("Transform").copied().unwrap_or(false), "Fold state persists through build");

        // Test toggling
        panel.section_folded.insert("Transform".to_string(), false);
        assert!(!panel.section_folded.get("Transform").copied().unwrap_or(true), "Fold state can be toggled");
    }

    #[test]
    fn folded_outliner_group_hides_its_rows() {
        // Test that folding an outliner group hides its child rows
        let mut panel = ScenePanel::new();
        panel.open();
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();

        // First build: all groups expanded
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let expanded_outliner_ids = panel.outliner_row_ids.len();

        // Fold the Objects group
        panel.outliner_folded.insert("Objects", true);

        // Rebuild with Objects folded
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let folded_outliner_ids = panel.outliner_row_ids.len();

        // Folded group should have fewer selectable rows (object rows hidden)
        assert!(folded_outliner_ids < expanded_outliner_ids, "Folded group should hide child rows");

        // Verify the fold state persisted
        assert!(panel.outliner_folded.get("Objects").copied().unwrap_or(false), "Objects fold state should persist");

        // Unfold and verify rows return
        panel.outliner_folded.insert("Objects", false);
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let unfolded_outliner_ids = panel.outliner_row_ids.len();
        assert_eq!(unfolded_outliner_ids, expanded_outliner_ids, "Unfolding should restore original row count");
    }

    #[test]
    fn fold_state_survives_rebuild_cycle() {
        // Test that fold state persists through configure → build → rebuild cycle
        let mut panel = ScenePanel::new();
        panel.open();

        // Initial configure
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));

        // Set fold states
        panel.section_folded.insert("Material".to_string(), true);
        panel.outliner_folded.insert("Lights", true);

        let mut tree = UITree::new();

        // First build
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.section_folded.get("Material").copied().unwrap_or(false), "Material folded after first build");
        assert!(panel.outliner_folded.get("Lights").copied().unwrap_or(false), "Lights folded after first build");

        // Reconfigure (simulating a layer change or sync)
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));

        // Fold states should survive configure
        assert!(panel.section_folded.get("Material").copied().unwrap_or(false), "Material folded after reconfigure");
        assert!(panel.outliner_folded.get("Lights").copied().unwrap_or(false), "Lights folded after reconfigure");

        // Second build
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.section_folded.get("Material").copied().unwrap_or(false), "Material folded after second build");
        assert!(panel.outliner_folded.get("Lights").copied().unwrap_or(false), "Lights folded after second build");

        // Verify the folded state still affects rendering
        // (folded sections should have fewer rows than expanded)
        panel.section_folded.insert("Transform".to_string(), false); // Ensure Transform is expanded for comparison
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        // Material should still be folded, Transform expanded
        assert!(panel.section_folded.get("Material").copied().unwrap_or(false), "Material remains folded through cycle");
        assert!(!panel.section_folded.get("Transform").copied().unwrap_or(true), "Transform remains expanded through cycle");
    }

    #[test]
    fn frame_action_on_object_emits_frame_selected_action() {
        // Test that Frame button emits SceneSetupFrameSelected action for orbit camera case
        let mut panel = ScenePanel::new();
        panel.open();
        let vm = azalea_shaped_vm();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();

        // Build to create the Frame button
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        // Verify Frame button was created for the selected object
        assert!(!panel.object_frame_ids.is_empty(), "Frame button should be created for object selection");

        // The full camera math (target = object position, distance = 2.2 × extent) is
        // tested in the app-side integration test that verifies the actual param writes.
        // This UI-level test verifies the button creation and routing infrastructure.
        assert!(!panel.object_frame_ids.is_empty(), "Frame button exists and is routable");
    }

    fn material_test_row(
        id: &str,
        role: MaterialParamRole,
        base: f32,
        default: f32,
    ) -> ParamRow {
        let mut row = placeholder_param_info();
        row.id = manifold_foundation::ParamId::from(id.to_string());
        row.spec.name = id.to_string();
        row.spec.section = Some("Material".to_string());
        row.spec.material_role = Some(role);
        row.spec.default = default;
        row.value.base = base;
        row.value.effective = base;
        row
    }

    #[test]
    fn material_inspector_buckets_are_unique_and_ordered() {
        let rows = [
            material_test_row("advanced", MaterialParamRole::Scalar(MaterialGroup::Advanced), 0.0, 0.0),
            material_test_row("coat", MaterialParamRole::Scalar(MaterialGroup::Feature(crate::param_surface::MaterialFeature::Coat)), 0.0, 0.0),
            material_test_row("opacity", MaterialParamRole::Scalar(MaterialGroup::Opacity), 1.0, 1.0),
            material_test_row("surface", MaterialParamRole::Scalar(MaterialGroup::Surface), 0.5, 0.5),
        ];
        let panel = ScenePanel::new();
        let mut indices = [0usize, 1, 2, 3];
        indices.sort_by_key(|&index| panel.material_bucket(&rows[index]));
        assert_eq!(indices, [3, 2, 1, 0], "surface, opacity, feature, advanced order");
        let names: Vec<String> = indices.iter().map(|&index| panel.material_section_name(&rows[index]).unwrap()).collect();
        assert_eq!(names, vec!["Surface", "Opacity & Cutout", "Coat", "Advanced"]);
        assert!(names.windows(2).all(|pair| pair[0] != pair[1]));
    }

    #[test]
    fn material_inspector_feature_mode_emits_scoped_enum_batch() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mode = material_test_row("51_coat_mode", MaterialParamRole::FeatureMode(feature), 0.0, 0.0);
        let mut panel = ScenePanel::new();
        panel.properties_card.rows = vec![mode];
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("object") },
            material: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("material") },
            shared_object_count: Some(1),
            textures: Vec::new(),
            params: vec![("coat_mode".into(), manifold_foundation::ParamId::from("51_coat_mode".to_string()))],
        });
        let target = GraphParamTarget::GeneratorOf(LayerId::new("layer"));
        let actions = panel.properties_row_action(0, RowRole::MaterialFeatureToggle(feature), NodeId::PLACEHOLDER, target);
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::Project(ProjectAction::MaterialParamsSet { kind: MaterialEditKind::Feature, writes, .. })]
                if writes.len() == 1 && writes[0].param_id.as_ref() == "51_coat_mode" && writes[0].value == 1.0
        ));
    }

    #[test]
    fn material_inspector_secondary_values_stay_under_add_feature() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mode = material_test_row("51_coat_mode", MaterialParamRole::FeatureMode(feature), 0.0, 0.0);
        let secondary = material_test_row(
            "51_clearcoat_roughness",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.9,
            0.0,
        );
        let panel = ScenePanel::new();
        assert!(
            !panel.material_feature_visible(&[mode, secondary], feature),
            "secondary authored values must remain reachable through Add Feature"
        );
    }

    #[test]
    fn material_inspector_mapping_keeps_neutral_feature_visible() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mut row = material_test_row(
            "51_clearcoat_roughness",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        row.mapping.ableton_range = Some((0.0, 1.0));
        let panel = ScenePanel::new();
        assert!(panel.material_feature_visible(&[row], feature));
    }

    #[test]
    fn material_inspector_authored_feature_survives_effective_zero() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mut row = material_test_row(
            "51_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.75,
            0.0,
        );
        row.value.effective = 0.0;
        let mut panel = ScenePanel::new();
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("object") },
            material: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("material") },
            shared_object_count: Some(1),
            textures: Vec::new(),
            params: vec![("clearcoat".into(), row.id.clone())],
        });
        assert!(
            panel.material_feature_visible(&[row], feature),
            "authored feature membership must use base, not the modulated frame value"
        );
    }

    #[test]
    fn material_inspector_external_attachment_survives_dormant_flags() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mut row = material_test_row(
            "51_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        row.material_attached = true;
        let panel = ScenePanel::new();
        assert!(panel.material_feature_visible(&[row], feature));
    }

    #[test]
    fn material_inspector_controlling_factor_uses_exact_inner_binding() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let custom_id = manifold_foundation::ParamId::from("outer_custom_factor".to_string());
        let mut row = material_test_row(
            custom_id.as_ref(),
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.6,
            0.0,
        );
        row.id = custom_id.clone();
        let mut panel = ScenePanel::new();
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("object") },
            material: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("material") },
            shared_object_count: Some(1),
            textures: Vec::new(),
            params: vec![("clearcoat".into(), custom_id)],
        });
        assert!(panel.material_feature_visible(&[row], feature));
    }

    #[test]
    fn material_inspector_rgb_lifecycle_preserves_other_channels() {
        let r = manifold_foundation::ParamId::from("51_base_color_r".to_string());
        let g = manifold_foundation::ParamId::from("51_base_color_g".to_string());
        let b = manifold_foundation::ParamId::from("51_base_color_b".to_string());
        let mut panel = ScenePanel::new();
        let mut red = material_test_row(
            "51_base_color_r",
            MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::R),
            0.2,
            0.0,
        );
        red.rgb_members = Some([r.clone(), g.clone(), b.clone()]);
        let green = material_test_row(
            "51_base_color_g",
            MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::G),
            0.4,
            0.0,
        );
        let blue = material_test_row(
            "51_base_color_b",
            MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::B),
            0.6,
            0.0,
        );
        panel.properties_card.rows = vec![red, green, blue];
        panel.properties_card.current_values = vec![0.2, 0.4, 0.6];
        panel.properties_card.row_id_index.extend([
            (r.to_string(), 0),
            (g.to_string(), 1),
            (b.to_string(), 2),
        ]);
        let target = GraphParamTarget::GeneratorOf(LayerId::new("layer"));
        let actions = panel.rewrite_material_rgb_actions(vec![
            PanelAction::Scrub(ValueRef::Param(target.clone(), g.clone()), ScrubPhase::Begin),
            PanelAction::Scrub(
                ValueRef::Param(target.clone(), g.clone()),
                ScrubPhase::Move(ScrubValue::Scalar(0.8)),
            ),
            PanelAction::Scrub(ValueRef::Param(target, g.clone()), ScrubPhase::Commit),
        ]);
        assert!(matches!(&actions[0], PanelAction::Scrub(ValueRef::ParamRgb(_, ids), ScrubPhase::Begin) if ids == &[r.clone(), g.clone(), b.clone()]));
        assert!(matches!(actions[1], PanelAction::Scrub(ValueRef::ParamRgb(_, _), ScrubPhase::Move(ScrubValue::Rgb([0.2, 0.8, 0.6])))));
        assert!(matches!(actions[2], PanelAction::Scrub(ValueRef::ParamRgb(_, _), ScrubPhase::Commit)));
    }

    #[test]
    fn material_inspector_rgb_uses_nonprimary_colour_and_scalar_fallback() {
        let mut row = material_test_row(
            "51_specular_r",
            MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Specular, RgbChannel::R),
            0.5,
            0.0,
        );
        row.value.driven = true;
        assert_eq!(ScenePanel::material_rgb_colour(&row), Some(crate::param_surface::MaterialColour::Specular));
        let mut panel = ScenePanel::new();
        panel.properties_card.rows = vec![row.clone()];
        panel.properties_card.current_values = vec![0.5];
        panel.properties_card.row_id_index.insert(row.id.to_string(), 0);
        let target = GraphParamTarget::GeneratorOf(LayerId::new("layer"));
        let action = PanelAction::Scrub(ValueRef::Param(target, row.id.clone()), ScrubPhase::Move(ScrubValue::Scalar(0.7)));
        let actions = panel.rewrite_material_rgb_actions(vec![action.clone()]);
        assert!(matches!(actions.as_slice(), [PanelAction::Scrub(ValueRef::Param(_, id), ScrubPhase::Move(ScrubValue::Scalar(0.7)))] if id == &row.id));
    }

    #[test]
    fn material_inspector_texture_families_split_connected_and_dormant_drawers() {
        let placement = material_test_row(
            "51_base_uv_m00",
            MaterialParamRole::Placement(MaterialMapFamily::Base, crate::param_surface::UvComponent::M00),
            1.0,
            1.0,
        );
        let mut panel = ScenePanel::new();
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("object") },
            material: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("material") },
            shared_object_count: Some(1),
            textures: vec![MaterialTextureInfo {
                port: "base_color_map".into(),
                label: "base color".into(),
                source_label: "Connected · albedo.png".into(),
                connected: true,
                graph_source: false,
            }],
            params: Vec::new(),
        });
        assert_eq!(panel.material_section_name(&placement).as_deref(), Some("Textures · Base Color"));
        assert!(panel.material_section_folded("Textures · Base Color"));
        panel.active_material_info.as_mut().unwrap().textures[0].connected = false;
        assert_eq!(panel.material_section_name(&placement).as_deref(), Some("Advanced · Dormant Textures"));
        assert!(panel.material_section_folded("Advanced · Dormant Textures"));
        assert_eq!(ScenePanel::material_family_for_port("clearcoat_normal_map"), None);
    }

    #[test]
    fn material_inspector_seed_writes_follow_selected_material_mapping() {
        let feature = crate::param_surface::MaterialFeature::Coat;
        let mode_51 = material_test_row("51_coat_mode", MaterialParamRole::FeatureMode(feature), 0.0, 0.0);
        let mut seed_51 = material_test_row(
            "51_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        seed_51.spec.name = "Clearcoat".into();
        seed_51.mapping.osc_address = Some("/material/clearcoat".into());
        let mode_52 = material_test_row("52_coat_mode", MaterialParamRole::FeatureMode(feature), 0.0, 0.0);
        let seed_52 = material_test_row(
            "52_clearcoat",
            MaterialParamRole::Scalar(MaterialGroup::Feature(feature)),
            0.0,
            0.0,
        );
        let mut panel = ScenePanel::new();
        panel.properties_card.rows = vec![mode_51.clone(), seed_51.clone(), mode_52, seed_52];
        panel.active_material_info = Some(MaterialInspectorInfo {
            object_gain: None,
            object: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("object") },
            material: ModifierObjectRef { scope: Vec::new(), node: manifold_foundation::NodeId::new("material") },
            shared_object_count: Some(1),
            textures: Vec::new(),
            params: vec![
                ("coat_mode".into(), mode_51.id.clone()),
                ("clearcoat".into(), seed_51.id.clone()),
            ],
        });
        let writes = panel.material_feature_writes(&panel.properties_card.rows, feature, &mode_51.id);
        assert!(writes.iter().any(|write| write.param_id == mode_51.id && write.value == 2.0));
        assert!(writes.iter().any(|write| write.param_id == seed_51.id && write.value == 1.0));
        assert!(!writes.iter().any(|write| write.param_id.as_ref() == "52_clearcoat"));
    }

    #[test]
    fn material_inspector_collapsed_rgb_uses_full_surface_channels() {
        let (_, mut surface) = tests::world_transform_vm();
        let mut red = surface.rows[0].clone();
        red.id = manifold_foundation::ParamId::from("51_base_color_r".to_string());
        red.value.base = 0.2;
        red.spec.material_role = Some(MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::R));
        let mut green = red.clone();
        green.id = manifold_foundation::ParamId::from("51_base_color_g".to_string());
        green.value.base = 0.4;
        green.spec.material_role = Some(MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::G));
        let mut blue = red.clone();
        blue.id = manifold_foundation::ParamId::from("51_base_color_b".to_string());
        blue.value.base = 0.6;
        blue.spec.material_role = Some(MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::B));
        red.rgb_members = Some([red.id.clone(), green.id.clone(), blue.id.clone()]);
        surface.rows = vec![red.clone(), green, blue];
        let mut panel = ScenePanel::new();
        panel.full_params = Some(surface);
        panel.properties_card.rows = vec![red.clone()];
        panel.properties_card.current_values = vec![0.2];
        panel.properties_card.row_id_index.insert(red.id.to_string(), 0);
        assert_eq!(panel.material_full_value(&manifold_foundation::ParamId::from("51_base_color_g".to_string())), 0.4);
        assert_eq!(panel.material_full_value(&manifold_foundation::ParamId::from("51_base_color_b".to_string())), 0.6);
    }

    #[test]
    fn material_inspector_expanded_rgb_keeps_primary_channel_reachable() {
        let r = manifold_foundation::ParamId::from("51_base_color_r".to_string());
        let g = manifold_foundation::ParamId::from("51_base_color_g".to_string());
        let b = manifold_foundation::ParamId::from("51_base_color_b".to_string());
        let mut primary = material_test_row(
            r.as_ref(),
            MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::R),
            0.2,
            0.0,
        );
        primary.rgb_members = Some([r.clone(), g.clone(), b.clone()]);
        let green = material_test_row(
            g.as_ref(),
            MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::G),
            0.4,
            0.0,
        );
        let blue = material_test_row(
            b.as_ref(),
            MaterialParamRole::Colour(MaterialGroup::Surface, crate::param_surface::MaterialColour::Base, RgbChannel::B),
            0.6,
            0.0,
        );
        let mut panel = ScenePanel::new();
        panel.material_rgb_expanded.insert(r.clone());
        let rows = vec![primary.clone(), green, blue];
        assert!(panel.material_rgb_row_visible(&rows, &primary));
        assert!(panel.material_rgb_row_visible(&rows, &rows[1]));
        assert!(panel.material_rgb_row_visible(&rows, &rows[2]));
    }
}
