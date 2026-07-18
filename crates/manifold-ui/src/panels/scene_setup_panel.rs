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
//! shared-lock wrapper types appear anywhere in this file (§4 negative gate).

use crate::chrome::{ChromeHost, Pad, Sizing, View};
use crate::color;
use crate::input::UIEvent;
use crate::node::*;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};
use crate::tree::UITree;
use manifold_foundation::{AudioSendId, LayerId};

use super::{GraphParamTarget, PanelAction};
use super::drawer::DrawerIds;
use super::param_card::{ParamInfo, RowGeometry};
use super::param_slider_shared::{
    AbletonConfigIds, DriverConfigIds, EnvelopeConfigIds, EnvelopeTargetIds, ModTab, ParamModState,
    RowClick, TrimHandleIds, build_param_row, enum_value_cell_actions, match_param_row_click,
};

// ── Stable keys ──
const KEY_BG: u64 = 80_001;
const KEY_CLOSE: u64 = 80_002;
const KEY_ADD_ENVIRONMENT: u64 = 80_010;
const KEY_ADD_FOG: u64 = 80_011;
const KEY_NEW_SCENE: u64 = 80_012;
const KEY_OPEN_GRAPH_EDITOR: u64 = 80_013;
const KEY_ADD_OBJECT: u64 = 80_014;
const KEY_ADD_LIGHT: u64 = 80_015;
/// "Import Model…" (P4, D4/D5) — merges a second glb into this scene.
const KEY_IMPORT_MODEL: u64 = 80_016;

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


// C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): the Light family's own
// FIXED slot indices into `self.light_card` — same identity-stability
// rationale as `OBJ_ROW_POS_X`.. above. Only ONE light's Properties body
// renders at a time (the outliner selection), same as Object.
const LIGHT_ROW_MODE: usize = 0;
const LIGHT_ROW_COLOR_R: usize = 1;
const LIGHT_ROW_COLOR_G: usize = 2;
const LIGHT_ROW_COLOR_B: usize = 3;
const LIGHT_ROW_INTENSITY: usize = 4;
const LIGHT_ROW_POS_X: usize = 5;
const LIGHT_ROW_POS_Y: usize = 6;
const LIGHT_ROW_POS_Z: usize = 7;
const LIGHT_ROW_AIM_X: usize = 8;
const LIGHT_ROW_AIM_Y: usize = 9;
const LIGHT_ROW_AIM_Z: usize = 10;
const LIGHT_ROW_CAST_SHADOWS: usize = 11;
const LIGHT_ROW_SHADOW_SOFTNESS: usize = 12;
const LIGHT_ROW_LIGHT_SIZE: usize = 13;
const LIGHT_ROW_COUNT: usize = 14;

/// Stable automation name for one Light row's value-TEXT cell — reuses the
/// exact strings the old bespoke stepper/triplet rows already published.
const fn light_row_value_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        LIGHT_ROW_MODE => Some("scene_setup.light.mode_value"),
        LIGHT_ROW_COLOR_R => Some("scene_setup.light.color_r"),
        LIGHT_ROW_COLOR_G => Some("scene_setup.light.color_g"),
        LIGHT_ROW_COLOR_B => Some("scene_setup.light.color_b"),
        LIGHT_ROW_INTENSITY => Some("scene_setup.light.intensity_value"),
        LIGHT_ROW_POS_X => Some("scene_setup.light.pos_x"),
        LIGHT_ROW_POS_Y => Some("scene_setup.light.pos_y"),
        LIGHT_ROW_POS_Z => Some("scene_setup.light.pos_z"),
        LIGHT_ROW_AIM_X => Some("scene_setup.light.aim_x"),
        LIGHT_ROW_AIM_Y => Some("scene_setup.light.aim_y"),
        LIGHT_ROW_AIM_Z => Some("scene_setup.light.aim_z"),
        LIGHT_ROW_CAST_SHADOWS => Some("scene_setup.light.cast_shadows_value"),
        LIGHT_ROW_SHADOW_SOFTNESS => Some("scene_setup.light.shadow_softness_value"),
        LIGHT_ROW_LIGHT_SIZE => Some("scene_setup.light.light_size_value"),
        _ => None,
    }
}

/// C-P1c: stable automation name for one Light row's SLIDER TRACK — mirrors
/// `object_row_track_automation_name`.
const fn light_row_track_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        LIGHT_ROW_MODE => Some("scene_setup.light.mode_track"),
        LIGHT_ROW_COLOR_R => Some("scene_setup.light.color_r_track"),
        LIGHT_ROW_COLOR_G => Some("scene_setup.light.color_g_track"),
        LIGHT_ROW_COLOR_B => Some("scene_setup.light.color_b_track"),
        LIGHT_ROW_INTENSITY => Some("scene_setup.light.intensity_track"),
        LIGHT_ROW_POS_X => Some("scene_setup.light.pos_x_track"),
        LIGHT_ROW_POS_Y => Some("scene_setup.light.pos_y_track"),
        LIGHT_ROW_POS_Z => Some("scene_setup.light.pos_z_track"),
        LIGHT_ROW_AIM_X => Some("scene_setup.light.aim_x_track"),
        LIGHT_ROW_AIM_Y => Some("scene_setup.light.aim_y_track"),
        LIGHT_ROW_AIM_Z => Some("scene_setup.light.aim_z_track"),
        LIGHT_ROW_CAST_SHADOWS => Some("scene_setup.light.cast_shadows_track"),
        LIGHT_ROW_SHADOW_SOFTNESS => Some("scene_setup.light.shadow_softness_track"),
        LIGHT_ROW_LIGHT_SIZE => Some("scene_setup.light.light_size_track"),
        _ => None,
    }
}

/// C-P1c: stable automation name for one Light row's driver ("D") arm
/// button — mirrors `object_row_driver_btn_automation_name`.
const fn light_row_driver_btn_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        LIGHT_ROW_INTENSITY => Some("scene_setup.light.intensity_driver_btn"),
        LIGHT_ROW_POS_X => Some("scene_setup.light.pos_x_driver_btn"),
        LIGHT_ROW_POS_Y => Some("scene_setup.light.pos_y_driver_btn"),
        LIGHT_ROW_POS_Z => Some("scene_setup.light.pos_z_driver_btn"),
        LIGHT_ROW_AIM_X => Some("scene_setup.light.aim_x_driver_btn"),
        LIGHT_ROW_AIM_Y => Some("scene_setup.light.aim_y_driver_btn"),
        LIGHT_ROW_AIM_Z => Some("scene_setup.light.aim_z_driver_btn"),
        LIGHT_ROW_CAST_SHADOWS => Some("scene_setup.light.cast_shadows_driver_btn"),
        LIGHT_ROW_SHADOW_SOFTNESS => Some("scene_setup.light.shadow_softness_driver_btn"),
        LIGHT_ROW_LIGHT_SIZE => Some("scene_setup.light.light_size_driver_btn"),
        _ => None,
    }
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

/// C-P1d: the slot offset `build_modifier_card_row` adds before shifting
/// into `build_param_row`'s `row_key_base` — pushes the WHOLE
/// `modifier_card` key range far clear of `object_card`'s (0..`OBJ_ROW_COUNT`
/// before its own `<< 8`), since Modifier's rows build as siblings of
/// Object's under the SAME `content_parent` every frame (the modifier stack
/// lives inside the selected object's properties body, unlike World/Light/
/// Camera which are mutually-exclusive Properties-body sections that never
/// coexist with Object in one frame). Chosen well above any realistic
/// `object_card`/`light_card`/`camera_card` slot count and far below
/// `modifier_row_key`'s own explicit-key range (`MODIFIER_KEY_BASE`'s
/// `<< 0`, not `<< 8` — the two schemes never overlap by construction even
/// without this margin, but the margin makes it visibly obvious on
/// inspection too).
const MODIFIER_CARD_ROW_KEY_OFFSET: u64 = 100_000;

const fn modifier_add_button_key(object_index: usize) -> u64 {
    MODIFIER_KEY_BASE + object_index as u64 * MODIFIER_OBJ_STRIDE + MODIFIER_ADD_BUTTON_OFFSET
}

/// Stable automation name for a modifier param row's value cell, by its slot
/// within THAT modifier's own param list (0-based — the widest atom,
/// `push_mesh`, has 4 params). `scripts/ui-flows/` selects the first
/// modifier's params via `nth` on this name, same "name over raw pixel
/// coordinate" convention the P4 lesson calls out — a fixed selector survives
/// a row shifting position when other panel content changes above it.
const fn modifier_param_row_automation_name(param_slot: usize) -> Option<&'static str> {
    match param_slot {
        0 => Some("scene_setup.modifier.param0_value"),
        1 => Some("scene_setup.modifier.param1_value"),
        2 => Some("scene_setup.modifier.param2_value"),
        3 => Some("scene_setup.modifier.param3_value"),
        _ => None,
    }
}


/// C-P1d (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): stable automation name
/// for a modifier param row's SLIDER TRACK, by param slot — the drag target
/// once Modifier moves onto the card row's own `SliderDragState` protocol
/// (`SliderDragState::try_start_drag` only arms on `ids.track`, never
/// `ids.value_text` — same split World/Object/Light/Camera already have
/// between their own `*_value`/`*_track` automation names).
const fn modifier_param_track_automation_name(param_slot: usize) -> Option<&'static str> {
    match param_slot {
        0 => Some("scene_setup.modifier.param0_track"),
        1 => Some("scene_setup.modifier.param1_track"),
        2 => Some("scene_setup.modifier.param2_track"),
        3 => Some("scene_setup.modifier.param3_track"),
        _ => None,
    }
}

/// C-P1d: stable automation name for a modifier param row's driver button,
/// by param slot — same convention as the track/value names above, kept for
/// the same "one interaction model to learn" parity World/Object/Light/
/// Camera's own `*_driver_btn` names already give those families.
const fn modifier_param_driver_btn_automation_name(param_slot: usize) -> Option<&'static str> {
    match param_slot {
        0 => Some("scene_setup.modifier.param0_driver_btn"),
        1 => Some("scene_setup.modifier.param1_driver_btn"),
        2 => Some("scene_setup.modifier.param2_driver_btn"),
        3 => Some("scene_setup.modifier.param3_driver_btn"),
        _ => None,
    }
}

// C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): the Camera family's own
// FIXED slot indices into `self.camera_card` — sized to the UNION of every
// field across the three curated camera atoms (Orbit/Free/LookAt), same
// "fixed superset, populate what applies" convention `OBJ_ROW_*` uses for
// Object's material section. Camera has exactly one row set per scene (no
// per-index stride).
const CAM_ROW_ORBIT: usize = 0;
const CAM_ROW_TILT: usize = 1;
const CAM_ROW_DISTANCE: usize = 2;
const CAM_ROW_POS_X: usize = 3;
const CAM_ROW_POS_Y: usize = 4;
const CAM_ROW_POS_Z: usize = 5;
const CAM_ROW_YAW: usize = 6;
const CAM_ROW_PITCH: usize = 7;
const CAM_ROW_ROLL: usize = 8;
const CAM_ROW_TARGET_X: usize = 9;
const CAM_ROW_TARGET_Y: usize = 10;
const CAM_ROW_TARGET_Z: usize = 11;
const CAM_ROW_FOV: usize = 12;
const CAM_ROW_LENS_FOCUS: usize = 13;
const CAM_ROW_LENS_FSTOP: usize = 14;
const CAM_ROW_LENS_SHUTTER: usize = 15;
const CAM_ROW_LENS_EXPOSURE: usize = 16;
const CAM_ROW_COUNT: usize = 17;

/// Stable automation name for one Camera row's value-TEXT cell — reuses the
/// exact strings the old bespoke stepper rows already published.
const fn camera_row_value_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        CAM_ROW_ORBIT => Some("scene_setup.camera.orbit_value"),
        CAM_ROW_TILT => Some("scene_setup.camera.tilt_value"),
        CAM_ROW_DISTANCE => Some("scene_setup.camera.distance_value"),
        CAM_ROW_POS_X => Some("scene_setup.camera.pos_x"),
        CAM_ROW_POS_Y => Some("scene_setup.camera.pos_y"),
        CAM_ROW_POS_Z => Some("scene_setup.camera.pos_z"),
        CAM_ROW_YAW => Some("scene_setup.camera.yaw_value"),
        CAM_ROW_PITCH => Some("scene_setup.camera.pitch_value"),
        CAM_ROW_ROLL => Some("scene_setup.camera.roll_value"),
        CAM_ROW_TARGET_X => Some("scene_setup.camera.target_x"),
        CAM_ROW_TARGET_Y => Some("scene_setup.camera.target_y"),
        CAM_ROW_TARGET_Z => Some("scene_setup.camera.target_z"),
        CAM_ROW_FOV => Some("scene_setup.camera.fov_y_value"),
        CAM_ROW_LENS_FOCUS => Some("scene_setup.camera.lens_focus_distance_value"),
        CAM_ROW_LENS_FSTOP => Some("scene_setup.camera.lens_f_stop_value"),
        CAM_ROW_LENS_SHUTTER => Some("scene_setup.camera.lens_shutter_angle_value"),
        CAM_ROW_LENS_EXPOSURE => Some("scene_setup.camera.lens_exposure_ev_value"),
        _ => None,
    }
}

/// C-P1c: stable automation name for one Camera row's SLIDER TRACK — mirrors
/// `object_row_track_automation_name`.
const fn camera_row_track_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        CAM_ROW_ORBIT => Some("scene_setup.camera.orbit_track"),
        CAM_ROW_TILT => Some("scene_setup.camera.tilt_track"),
        CAM_ROW_DISTANCE => Some("scene_setup.camera.distance_track"),
        CAM_ROW_POS_X => Some("scene_setup.camera.pos_x_track"),
        CAM_ROW_POS_Y => Some("scene_setup.camera.pos_y_track"),
        CAM_ROW_POS_Z => Some("scene_setup.camera.pos_z_track"),
        CAM_ROW_YAW => Some("scene_setup.camera.yaw_track"),
        CAM_ROW_PITCH => Some("scene_setup.camera.pitch_track"),
        CAM_ROW_ROLL => Some("scene_setup.camera.roll_track"),
        CAM_ROW_TARGET_X => Some("scene_setup.camera.target_x_track"),
        CAM_ROW_TARGET_Y => Some("scene_setup.camera.target_y_track"),
        CAM_ROW_TARGET_Z => Some("scene_setup.camera.target_z_track"),
        CAM_ROW_FOV => Some("scene_setup.camera.fov_y_track"),
        CAM_ROW_LENS_FOCUS => Some("scene_setup.camera.lens_focus_distance_track"),
        CAM_ROW_LENS_FSTOP => Some("scene_setup.camera.lens_f_stop_track"),
        CAM_ROW_LENS_SHUTTER => Some("scene_setup.camera.lens_shutter_angle_track"),
        CAM_ROW_LENS_EXPOSURE => Some("scene_setup.camera.lens_exposure_ev_track"),
        _ => None,
    }
}

/// C-P1c: stable automation name for one Camera row's driver ("D") arm
/// button — mirrors `object_row_driver_btn_automation_name`.
const fn camera_row_driver_btn_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        CAM_ROW_ORBIT => Some("scene_setup.camera.orbit_driver_btn"),
        _ => None,
    }
}


/// `is_angle` for a Camera row, by slot (D10's committed degrees-display
/// table: orbit/tilt/yaw/pitch/roll/fov_y).
const fn camera_row_is_angle(slot: usize) -> bool {
    matches!(slot, CAM_ROW_ORBIT | CAM_ROW_TILT | CAM_ROW_YAW | CAM_ROW_PITCH | CAM_ROW_ROLL | CAM_ROW_FOV)
}


// C-P1a (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): `SceneCardState`'s FIXED
// slot indices for the four Environment/Fog rows — stable across rebuilds
// regardless of which optional rows are present (see `SceneCardState`'s doc
// comment), so a widget's identity — and an armed driver/envelope/audio
// mod's slot — never shifts under the user's cursor mid-drag or when a
// sibling section is added/removed.
const WORLD_ENV_INTENSITY: usize = 0;
const WORLD_ENV_FILL: usize = 1;
const WORLD_FOG_DENSITY: usize = 2;
const WORLD_FOG_HEIGHT_FALLOFF: usize = 3;
const WORLD_ROW_COUNT: usize = 4;

/// Stable automation name for one of the four fixed rows' value-text cell —
/// `scripts/ui-flows/` selectors key on this instead of ambiguous
/// `text`/`under_text` queries.
const fn world_row_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        WORLD_ENV_INTENSITY => Some("scene_setup.environment.intensity_value"),
        WORLD_ENV_FILL => Some("scene_setup.environment.fill_value"),
        WORLD_FOG_DENSITY => Some("scene_setup.fog.density_value"),
        WORLD_FOG_HEIGHT_FALLOFF => Some("scene_setup.fog.height_falloff_value"),
        _ => None,
    }
}


/// C-P1a: stable automation name for one of the four fixed rows' SLIDER
/// TRACK — the real card drag target (`build_param_row`'s `BitmapSlider`),
/// distinct from `world_row_automation_name`'s value-TEXT cell (double-
/// click-to-type only, not drag-armable on a card slider). Flow scripts
/// that scrub a converted row target this name.
const fn world_row_track_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        WORLD_ENV_INTENSITY => Some("scene_setup.environment.intensity_track"),
        WORLD_ENV_FILL => Some("scene_setup.environment.fill_track"),
        WORLD_FOG_DENSITY => Some("scene_setup.fog.density_track"),
        WORLD_FOG_HEIGHT_FALLOFF => Some("scene_setup.fog.height_falloff_track"),
        _ => None,
    }
}

/// C-P1a: stable automation name for one of the four fixed rows' driver
/// ("D") arm button — opens the inline modulation drawer.
const fn world_row_driver_btn_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        WORLD_ENV_INTENSITY => Some("scene_setup.environment.intensity_driver_btn"),
        WORLD_ENV_FILL => Some("scene_setup.environment.fill_driver_btn"),
        WORLD_FOG_DENSITY => Some("scene_setup.fog.density_driver_btn"),
        WORLD_FOG_HEIGHT_FALLOFF => Some("scene_setup.fog.height_falloff_driver_btn"),
        _ => None,
    }
}

// C-P1b (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): the Object family's own
// FIXED slot indices into `self.object_card` — same identity-stability
// rationale as `WORLD_ENV_INTENSITY`..`WORLD_FOG_HEIGHT_FALLOFF` above (a
// driver/envelope/audio mod armed on Roughness can't silently jump onto
// Position X's slot just because the selected object's material section
// changed shape between frames). Sized to the union of every row an object
// can show — Position/Rotation/Scale (9) + Color (3) + Metallic + Roughness
// (2) — even though a given object may only populate a subset (`Other`/
// `None` material skip Metallic/Roughness; an object with no `transform`
// skips the first 9). Unpopulated slots for the CURRENT object are simply
// never written this frame (their `param_info`/`id_map` entries carry no
// row) — same "only every WIRED row lands in the map" contract D2 already
// established for World's `EnvironmentRowVm::None`/`Custom` cases.
const OBJ_ROW_POS_X: usize = 0;
const OBJ_ROW_POS_Y: usize = 1;
const OBJ_ROW_POS_Z: usize = 2;
const OBJ_ROW_ROT_X: usize = 3;
const OBJ_ROW_ROT_Y: usize = 4;
const OBJ_ROW_ROT_Z: usize = 5;
const OBJ_ROW_SCALE_X: usize = 6;
const OBJ_ROW_SCALE_Y: usize = 7;
const OBJ_ROW_SCALE_Z: usize = 8;
const OBJ_ROW_COLOR_R: usize = 9;
const OBJ_ROW_COLOR_G: usize = 10;
const OBJ_ROW_COLOR_B: usize = 11;
const OBJ_ROW_METALLIC: usize = 12;
const OBJ_ROW_ROUGHNESS: usize = 13;
const OBJ_ROW_COUNT: usize = 14;

/// Stable automation name for one Object row's value-TEXT cell (type-in
/// target) — reuses the EXACT strings the old bespoke triplet/slider rows
/// already published (`triplet_cell_automation_name`/
/// `object_slider_row_automation_name`), so existing flow-script selectors
/// that only assert/read this cell's text keep resolving unchanged.
const fn object_row_value_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        OBJ_ROW_POS_X => Some("scene_setup.object.pos_x"),
        OBJ_ROW_POS_Y => Some("scene_setup.object.pos_y"),
        OBJ_ROW_POS_Z => Some("scene_setup.object.pos_z"),
        OBJ_ROW_ROT_X => Some("scene_setup.object.rot_x"),
        OBJ_ROW_ROT_Y => Some("scene_setup.object.rot_y"),
        OBJ_ROW_ROT_Z => Some("scene_setup.object.rot_z"),
        OBJ_ROW_SCALE_X => Some("scene_setup.object.scale_x"),
        OBJ_ROW_SCALE_Y => Some("scene_setup.object.scale_y"),
        OBJ_ROW_SCALE_Z => Some("scene_setup.object.scale_z"),
        OBJ_ROW_COLOR_R => Some("scene_setup.object.color_r"),
        OBJ_ROW_COLOR_G => Some("scene_setup.object.color_g"),
        OBJ_ROW_COLOR_B => Some("scene_setup.object.color_b"),
        OBJ_ROW_METALLIC => Some("scene_setup.object.metallic_value"),
        OBJ_ROW_ROUGHNESS => Some("scene_setup.object.roughness_value"),
        _ => None,
    }
}

/// C-P1b: stable automation name for one Object row's SLIDER TRACK — the
/// real card drag target (mirrors `world_row_track_automation_name`). NEW
/// names (the old bespoke rows had no track — they were delta-dragged
/// directly off the value cell); flow scripts that used to drag the value
/// cell now target this name instead.
const fn object_row_track_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        OBJ_ROW_POS_X => Some("scene_setup.object.pos_x_track"),
        OBJ_ROW_POS_Y => Some("scene_setup.object.pos_y_track"),
        OBJ_ROW_POS_Z => Some("scene_setup.object.pos_z_track"),
        OBJ_ROW_ROT_X => Some("scene_setup.object.rot_x_track"),
        OBJ_ROW_ROT_Y => Some("scene_setup.object.rot_y_track"),
        OBJ_ROW_ROT_Z => Some("scene_setup.object.rot_z_track"),
        OBJ_ROW_SCALE_X => Some("scene_setup.object.scale_x_track"),
        OBJ_ROW_SCALE_Y => Some("scene_setup.object.scale_y_track"),
        OBJ_ROW_SCALE_Z => Some("scene_setup.object.scale_z_track"),
        OBJ_ROW_COLOR_R => Some("scene_setup.object.color_r_track"),
        OBJ_ROW_COLOR_G => Some("scene_setup.object.color_g_track"),
        OBJ_ROW_COLOR_B => Some("scene_setup.object.color_b_track"),
        OBJ_ROW_METALLIC => Some("scene_setup.object.metallic_track"),
        OBJ_ROW_ROUGHNESS => Some("scene_setup.object.roughness_track"),
        _ => None,
    }
}

/// C-P1b: stable automation name for one Object row's driver ("D") arm
/// button — mirrors `world_row_driver_btn_automation_name`.
const fn object_row_driver_btn_automation_name(slot: usize) -> Option<&'static str> {
    match slot {
        OBJ_ROW_POS_X => Some("scene_setup.object.pos_x_driver_btn"),
        OBJ_ROW_POS_Y => Some("scene_setup.object.pos_y_driver_btn"),
        OBJ_ROW_POS_Z => Some("scene_setup.object.pos_z_driver_btn"),
        OBJ_ROW_ROT_X => Some("scene_setup.object.rot_x_driver_btn"),
        OBJ_ROW_ROT_Y => Some("scene_setup.object.rot_y_driver_btn"),
        OBJ_ROW_ROT_Z => Some("scene_setup.object.rot_z_driver_btn"),
        OBJ_ROW_SCALE_X => Some("scene_setup.object.scale_x_driver_btn"),
        OBJ_ROW_SCALE_Y => Some("scene_setup.object.scale_y_driver_btn"),
        OBJ_ROW_SCALE_Z => Some("scene_setup.object.scale_z_driver_btn"),
        OBJ_ROW_COLOR_R => Some("scene_setup.object.color_r_driver_btn"),
        OBJ_ROW_COLOR_G => Some("scene_setup.object.color_g_driver_btn"),
        OBJ_ROW_COLOR_B => Some("scene_setup.object.color_b_driver_btn"),
        OBJ_ROW_METALLIC => Some("scene_setup.object.metallic_driver_btn"),
        OBJ_ROW_ROUGHNESS => Some("scene_setup.object.roughness_driver_btn"),
        _ => None,
    }
}


const PANEL_W_MIN: f32 = 320.0;
const TITLE_H: f32 = 26.0;
const ROW_H: f32 = 24.0;
const ROW_GAP: f32 = 4.0;
const PAD: f32 = 10.0;
const STEP_W: f32 = 22.0;
const LABEL_W: f32 = 130.0;
/// UX-P2 (D4/D7 of SCENE_PANEL_UX_DESIGN.md): the color row's live swatch —
/// the ONE new style constant that phase's §4 negative gate allowed. Sized
/// to sit inside a `ROW_H` row with visible margin top/bottom, echoing the
/// audio dock's identity swatch (`audio_setup_panel.rs`'s `SWATCH_W`) at a
/// square, not that dock's send-row proportions.
/// UX-P3a (D9's swatch polish, sizing amendment): bumped 14→20 — "reads as
/// a color chip, not a checkbox." The hairline border already existed
/// (`border_width: 1.0` in `build_color_row`); only the size needed fixing.
const SWATCH_W: f32 = 20.0;

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

/// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md D2: synthesize a converted scene
/// row's stable owned `ParamId` from its write address's `node_doc_id` +
/// the graph node's own param name. The ONE definition both sides of the
/// UI/app boundary use: `ScenePanel::build_world_param_row` (this crate)
/// calls it when building the row's `ParamInfo`/id map; `state_sync`'s VM
/// construction (`manifold-app`, no access to this synthesis logic
/// otherwise) calls it to look up that same row's driver/envelope/audio-mod
/// state on `PresetInstance` — a driver armed via `DriverToggle` is stored
/// keyed by exactly this string (`dispatch_inspector`'s modulation arms use
/// `pid_at(pi)` verbatim, unchanged by C-P1a), so the two call sites must
/// never drift.
pub fn synth_world_param_id(node_doc_id: u32, param_key: &str) -> manifold_foundation::ParamId {
    manifold_foundation::ParamId::from(format!("scene.{node_doc_id}.{param_key}"))
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
/// `(CardModulation, AudioCardState)` (both sized to 1) — this crate has no
/// `PresetInstance`, so the app computes this and hands it across the VM
/// boundary like every other field here. Field-for-field the same facts
/// `ParamModState`/`AudioCardState` carry per-row; a plain idle default
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
    pub envelope_active: bool,
    pub target_norm: f32,
    pub env_decay: f32,
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

/// One editable param row inside a modifier's own param set (D6: "the atom's
/// own params (amount/axis/center …) as ordinary editable rows"). `label` is
/// the primitive's own param label, transcribed by `state_sync` (this crate
/// can't depend on `manifold-renderer`'s `ParamDef`, same DTO-boundary
/// convention as `EnvironmentRowVm::mode_is_hdri`). `Axis` covers
/// Bend/Twist/Taper's own X/Y/Z selector — the same labeled-stepper shape
/// Light's Mode/Cast Shadows/Shadow Softness rows already ride through
/// `build_param_row`'s `value_labels` path, never a new widget kind.
/// C-P1d (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): both fields promoted to
/// `ModulatedRow`/`ModulatedEnumRow` — the same promotion C-P1b/C-P1c
/// already did for Object/Light — so Modifier's own rows build through
/// `build_modifier_card_row` (the shared card row core) instead of the
/// deleted pre-convergence bespoke numeric/enum stepper builders.
#[derive(Clone, Debug, PartialEq)]
pub enum ModifierParamRowVm {
    Numeric { label: &'static str, row: ModulatedRow },
    Axis { label: &'static str, row: ModulatedEnumRow },
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
    pub params: Vec<ModifierParamRowVm>,
}

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
/// `build_param_row`'s `ParamInfo.value_labels` path (the card row core
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
    /// C-P1a: every project audio send, card-level (same for every
    /// converted row on this layer) — the `AudioCardState.send_labels`/
    /// `send_ids` pair the shared `build_audio_mod_drawer`'s Source row
    /// needs. Mirrors `ParamCardConfig.audio.send_labels`/`send_ids`.
    pub audio_send_labels: Vec<String>,
    pub audio_send_ids: Vec<AudioSendId>,
    /// P2: the Objects section's rows, in `mesh_k` order.
    pub objects: Vec<ObjectRowVm>,
    /// P3: the Lights section's rows, in `light_k` order. Never capped —
    /// REALTIME_3D D4's shadow-caster limit (K=4) is the renderer's job; the
    /// panel reports the true count and renders every row regardless.
    pub lights: Vec<LightRowVm>,
    /// P3: the Camera section (D3's single-camera trace, lens pass-through
    /// included).
    pub camera: CameraRowVm,
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
/// How a driven row's read-only value label renders its number — captured at
/// build time so the per-frame value sync (`sync_row_values`) reproduces the
/// exact text the driven branch built, per row family (plain / degrees /
/// enum-labelled).
#[derive(Clone, Debug)]
enum DrivenFmt {
    Plain,
    Degrees,
    Labels(Vec<String>),
}

/// Per-frame sync handle for one driven (wire-fed) row: the value label's
/// tree node, the row's real write/read address, and its display format.
/// The driven branch used to discard the label `NodeId`, which froze driven
/// rows between structural syncs — this is the handle that unfreezes them.
#[derive(Clone, Debug)]
struct DrivenValueLabel {
    label: NodeId,
    addr: RowAddr,
    fmt: DrivenFmt,
}

fn driven_text(value: f32, fmt: &DrivenFmt) -> String {
    match fmt {
        DrivenFmt::Plain => format!("{value:.2} (driven)"),
        DrivenFmt::Degrees => format!("{:.1}\u{00b0} (driven)", value.to_degrees()),
        DrivenFmt::Labels(labels) => {
            let idx = value.round().clamp(0.0, (labels.len().max(1) - 1) as f32) as usize;
            format!("{} (driven)", labels.get(idx).map(String::as_str).unwrap_or("?"))
        }
    }
}

struct SceneCardState {
    param_info: Vec<ParamInfo>,
    mod_state: ParamModState,
    /// D2: synthesized owned `ParamId` → `(write address, snapshot value)`.
    /// Rebuilt fresh every `build_nodes` pass (D1 of SCENE_PANEL_UX_DESIGN.md:
    /// "no rotting, no staleness") — `dispatch_inspector`'s three insertion
    /// points resolve a card-shaped action's `param_id` through this before
    /// falling into `with_preset_graph_mut`.
    id_map: ahash::AHashMap<manifold_foundation::ParamId, (RowAddr, f32)>,
    slider_ids: Vec<Option<crate::slider::SliderNodeIds>>,
    /// Fixed-slot twin of `slider_ids`: the main slider's right-click reset
    /// (`build_param_row`'s `slider_reset`), replayed into node-intent
    /// dispatch by [`SceneCardState::register_intents`] — the same
    /// track+RightClick→reset contract every other slider in the app has.
    slider_resets: Vec<Option<PanelAction>>,
    /// Fixed-slot twin of `slider_ids` for DRIVEN rows: the read-only value
    /// label's sync handle. Exactly one of `slider_ids[slot]` /
    /// `driven_value_ids[slot]` is `Some` for a built row; both `None` for an
    /// unbuilt slot. Consumed by `sync_row_values` every frame.
    driven_value_ids: Vec<Option<DrivenValueLabel>>,
    row_catcher_ids: Vec<Option<NodeId>>,
    driver_btn_ids: Vec<Option<NodeId>>,
    envelope_btn_ids: Vec<Option<NodeId>>,
    driver_config_ids: Vec<Option<DriverConfigIds>>,
    /// Always `None` — no Ableton mapping surface on scene rows this phase
    /// (D1's `match_param_row_click` still takes the slice; an all-`None`
    /// vector is a legitimate "never active" input, not a stub).
    ableton_config_ids: Vec<Option<AbletonConfigIds>>,
    audio_btn_ids: Vec<Option<NodeId>>,
    audio_configs: Vec<Option<(DrawerIds, usize)>>,
    trim_ids: Vec<Option<TrimHandleIds>>,
    target_ids: Vec<Option<EnvelopeTargetIds>>,
    envelope_config_ids: Vec<Option<EnvelopeConfigIds>>,
    /// Always `None` — no OSC address surface on scene rows this phase.
    osc_addresses: Vec<Option<String>>,
    mod_tab_ids: Vec<Vec<(NodeId, ModTab)>>,
    mod_active_tab: Vec<ModTab>,
    /// One `SliderDragState` per row — the card drag protocol (D4): a
    /// track pointer-down snapshots + starts an absolute-position drag
    /// (mirrors `ScenePanel::metallic_slider`/`roughness_slider`, generalized
    /// to N rows), motion writes live, release commits ONE undo unit.
    drag_sliders: Vec<crate::slider::SliderDragState>,
}

impl SceneCardState {
    fn new() -> Self {
        Self {
            param_info: Vec::new(),
            mod_state: ParamModState::allocate(0),
            id_map: ahash::AHashMap::new(),
            slider_ids: Vec::new(),
            slider_resets: Vec::new(),
            driven_value_ids: Vec::new(),
            row_catcher_ids: Vec::new(),
            driver_btn_ids: Vec::new(),
            envelope_btn_ids: Vec::new(),
            driver_config_ids: Vec::new(),
            ableton_config_ids: Vec::new(),
            audio_btn_ids: Vec::new(),
            audio_configs: Vec::new(),
            trim_ids: Vec::new(),
            target_ids: Vec::new(),
            envelope_config_ids: Vec::new(),
            osc_addresses: Vec::new(),
            mod_tab_ids: Vec::new(),
            mod_active_tab: Vec::new(),
            drag_sliders: Vec::new(),
        }
    }

    /// Resize every per-row vector to `n`, rebuilding `mod_state` fresh (the
    /// build pass re-syncs every row's modulation facts from the VM's
    /// `RowModulation` every frame — same "no rotting" contract the rest of
    /// this panel already has, so nothing here needs to survive the
    /// resize). `mod_active_tab` and `drag_sliders` DO need to survive a
    /// mid-gesture rebuild (which mod tab is shown, an in-flight drag) — so
    /// unlike the display vectors above, these two are only ever GROWN, never
    /// truncated, even when `n` temporarily drops to 0 (a frame where World
    /// doesn't build at all, e.g. no selection) — truncating a `SliderDragState`
    /// mid-drag would silently drop the gesture the next time this panel
    /// re-opens. Same intent as `ParamCardPanel::configure`'s
    /// `mod_active_tab.resize(n, ..)`, adapted for this fixed-index family
    /// where `n` isn't monotonic across frames.
    fn resize(&mut self, n: usize) {
        self.param_info.resize(n, placeholder_param_info());
        self.mod_state = ParamModState::allocate(n);
        self.slider_ids.resize(n, None);
        self.slider_resets.resize_with(n, || None);
        self.driven_value_ids.resize_with(n, || None);
        self.row_catcher_ids.resize(n, None);
        self.driver_btn_ids.resize(n, None);
        self.envelope_btn_ids.resize(n, None);
        self.driver_config_ids.resize_with(n, || None);
        self.ableton_config_ids.resize_with(n, || None);
        self.audio_btn_ids.resize(n, None);
        self.audio_configs.resize_with(n, || None);
        self.trim_ids.resize_with(n, || None);
        self.target_ids.resize_with(n, || None);
        self.envelope_config_ids.resize_with(n, || None);
        self.osc_addresses.resize(n, None);
        self.mod_tab_ids.resize_with(n, Vec::new);
        while self.mod_active_tab.len() < n {
            self.mod_active_tab.push(ModTab::Driver);
        }
        while self.drag_sliders.len() < n {
            self.drag_sliders.push(crate::slider::SliderDragState::default());
        }
        self.id_map.clear();
    }

    /// Whether `node_id` is an enum (`value_labels`) row's value-text cell
    /// on this card — single click cycles/opens the dropdown (BUG-250), so
    /// double-click must NOT open the numeric type-in on the same cell.
    fn is_enum_value_cell(&self, node_id: NodeId) -> bool {
        self.slider_ids.iter().enumerate().any(|(pi, s)| {
            s.as_ref().is_some_and(|ids| ids.value_text == node_id)
                && self
                    .param_info
                    .get(pi)
                    .and_then(|p| p.value_labels.as_ref())
                    .is_some()
        })
    }

    fn pid_at(&self, i: usize) -> manifold_foundation::ParamId {
        self.param_info[i].param_id.clone()
    }

    /// Replay every materialised slider's `Track + RightClick → reset` intent
    /// — main rows plus the armed drawers' sliders (envelope Decay, audio
    /// Amount/Attack/Release). Mirrors `ParamCardPanel::register_intents`'s
    /// three loops; before this the scene cards built resets in
    /// `build_param_row` and silently dropped them (right-click reset dead on
    /// every scene row).
    fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        use crate::slider::BitmapSlider;
        for (pi, slider) in self.slider_ids.iter().enumerate() {
            if let (Some(ids), Some(reset)) =
                (slider, self.slider_resets.get(pi).and_then(|r| r.as_ref()))
            {
                BitmapSlider::register_track_reset(ids, reset, intents);
            }
        }
        for cfg in self.envelope_config_ids.iter().flatten() {
            BitmapSlider::register_track_reset(&cfg.decay_slider, &cfg.decay_reset, intents);
        }
        for (dids, _) in self.audio_configs.iter().flatten() {
            for (sl, reset) in dids.sliders.iter().zip(dids.slider_resets.iter()) {
                BitmapSlider::register_track_reset(sl, reset, intents);
            }
        }
    }

    /// BUG-250: map a [`RowClick::EnumValueCell`] hit to the shared
    /// cycle-or-dropdown action set (`enum_value_cell_actions`), targeting
    /// the layer's generator like every other scene-row card action. The
    /// current value comes from this pass's `id_map` snapshot (D1: rebuilt
    /// every build, never stale); the cell node id anchors the dropdown
    /// under the row's own value text.
    fn enum_value_cell_action(&self, i: usize, clicked: NodeId) -> Vec<PanelAction> {
        let info = &self.param_info[i];
        let labels = info.value_labels.clone().unwrap_or_default();
        let pid = self.pid_at(i);
        let value = self
            .id_map
            .get(&pid)
            .map(|(_, v)| *v)
            .unwrap_or(info.default);
        let cell = self
            .slider_ids
            .get(i)
            .and_then(|s| s.as_ref())
            .map(|s| s.value_text)
            .unwrap_or(clicked);
        enum_value_cell_actions(GraphParamTarget::Generator, pid, &labels, value, info.min, cell)
    }

    fn focus_mod_tab(&mut self, i: usize, tab: ModTab) {
        if let Some(slot) = self.mod_active_tab.get_mut(i) {
            *slot = tab;
        }
    }

    fn mod_tab_hit(&self, id: NodeId) -> Option<(usize, ModTab)> {
        self.mod_tab_ids.iter().enumerate().find_map(|(pi, tabs)| {
            tabs.iter().find(|(tid, _)| *tid == id).map(|&(_, t)| (pi, t))
        })
    }

    fn audio_toggle_action(&self, target: GraphParamTarget, pi: usize) -> Vec<PanelAction> {
        let ms = &self.mod_state;
        if ms.audio_active.get(pi).copied().unwrap_or(false) {
            vec![PanelAction::AudioModToggle(target, self.pid_at(pi))]
        } else if ms.audio_send_ids.is_empty() {
            vec![PanelAction::OpenAudioSetup]
        } else {
            vec![PanelAction::AudioModToggle(target, self.pid_at(pi))]
        }
    }

    fn audio_set_source_action(
        &self,
        target: GraphParamTarget,
        pi: usize,
        send_override: Option<usize>,
        kind_override: Option<usize>,
        band_override: Option<usize>,
    ) -> Vec<PanelAction> {
        use super::param_slider_shared::{audio_band_from_index, audio_kind_from_index};
        let ms = &self.mod_state;
        let send_k = send_override
            .map(|k| k as i32)
            .unwrap_or_else(|| ms.audio_send_idx.get(pi).copied().unwrap_or(-1));
        let Some(send_id) = (send_k >= 0)
            .then(|| ms.audio_send_ids.get(send_k as usize).cloned())
            .flatten()
        else {
            return vec![];
        };
        let kind_idx =
            kind_override.unwrap_or_else(|| ms.audio_kind_idx.get(pi).copied().unwrap_or(0) as usize);
        let band_idx =
            band_override.unwrap_or_else(|| ms.audio_band_idx.get(pi).copied().unwrap_or(0) as usize);
        let feature = crate::types::AudioFeature::new(
            audio_kind_from_index(kind_idx),
            audio_band_from_index(band_idx),
        );
        vec![PanelAction::AudioModSetSource(target, self.pid_at(pi), send_id, feature)]
    }
}

/// Placeholder `ParamInfo` used only to size `SceneCardState::resize`'s
/// grow step before the real per-row info is written by the build pass —
/// never observed by a click/drag (every live index is overwritten before
/// `build_nodes` returns).
fn placeholder_param_info() -> ParamInfo {
    ParamInfo {
        param_id: manifold_foundation::ParamId::from(""),
        name: String::new(),
        min: 0.0,
        max: 1.0,
        default: 0.0,
        whole_numbers: false,
        is_angle: false,
        exposed: false,
        is_toggle: false,
        is_trigger: false,
        is_trigger_gate: false,
        value_labels: None,
        osc_address: None,
        ableton_display: None,
        ableton_range: None,
        mappable: false,
        section: None,
    }
}

/// One numeric row's interactive node ids, set by `build_numeric_row` when
pub struct ScenePanel {
    open: bool,
    state: SceneSetupState,
    panel_w: f32,
    host: ChromeHost,
    scroll: ScrollContainer,
    content_parent: NodeId,
    bg_id: NodeId,
    close_id: NodeId,
    add_environment_id: Option<NodeId>,
    add_fog_id: Option<NodeId>,
    new_scene_id: Option<NodeId>,
    open_graph_editor_id: Option<NodeId>,
    /// C-P1a (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): the converted
    /// Environment/Fog family's card-shaped row state. Replaces the deleted
    /// `row_ids: [RowIds; 4]` — see `SceneCardState`'s doc comment for the
    /// fixed-slot convention.
    world_card: SceneCardState,
    /// C-P1b (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): the converted Object
    /// family's card-shaped row state — same `SceneCardState` shape as
    /// `world_card`, sized to `OBJ_ROW_COUNT` FIXED slots (`OBJ_ROW_POS_X`..
    /// `OBJ_ROW_ROUGHNESS`) rather than World's 4, since only ONE object's
    /// Properties body renders at a time (the outliner selection), same as
    /// World only ever shows one Environment/Fog section. Replaces the
    /// deleted `object_value_cells`/`metallic_slider`/`roughness_slider`.
    object_card: SceneCardState,
    /// C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): the converted Light
    /// family's card-shaped row state — same `SceneCardState` shape,
    /// `LIGHT_ROW_COUNT` fixed slots, one light's Properties body at a time.
    light_card: SceneCardState,
    /// C-P1c: the converted Camera family's card-shaped row state —
    /// `CAM_ROW_COUNT` fixed slots (the union of every field across the
    /// three curated camera atoms; only one row set exists per scene).
    camera_card: SceneCardState,
    /// C-P1d (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): the converted
    /// Modifier family's card-shaped row state — same `SceneCardState`
    /// shape, but VARIABLE slot count (unlike World/Object/Light/Camera's
    /// fixed unions): a modifier stack is a reorderable list of 0..N
    /// modifiers, each contributing its own curated param rows, so the
    /// selected object's `modifier_card` regrows every properties-body
    /// build to the ACTUAL total row count for this frame (`build_modifier_row`
    /// threads a running slot cursor across the stack). Row IDENTITY is
    /// still stable across frames despite the index not being: the
    /// synthesized `ParamId` (D2) encodes `(node_doc_id, param_key)`, not
    /// the slot index, so `resolve_scene_param`/the id-map never depend on
    /// slot stability — only `mod_active_tab`/`drag_sliders` (which
    /// `SceneCardState::resize` already grows-never-truncates) could, in
    /// principle, show the wrong row's drawer tab open for one frame after
    /// a reorder mid-drawer-open; accepted as a cosmetic edge case, same
    /// honesty standard as every other documented consequence in this file.
    modifier_card: SceneCardState,
    add_object_id: Option<NodeId>,
    add_light_id: Option<NodeId>,
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
    panel_rect: Rect,
    /// The layer_id a drag targets — captured at PointerDown so `on_event`
    /// doesn't need to re-read `self.state` (which may rebuild mid-drag on
    /// an unrelated `configure`, per D1 "no staleness": the drag itself
    /// still targets the layer it started on).
    drag_layer_id: Option<LayerId>,
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
            world_card: SceneCardState::new(),
            object_card: SceneCardState::new(),
            light_card: SceneCardState::new(),
            camera_card: SceneCardState::new(),
            modifier_card: SceneCardState::new(),
            add_object_id: None,
            add_light_id: None,
            import_model_id: None,
            selection: std::collections::HashMap::new(),
            outliner_row_ids: Vec::new(),
            outliner_eye_ids: Vec::new(),
            object_name_ids: Vec::new(),
            object_remove_ids: Vec::new(),
            object_duplicate_ids: Vec::new(),
            modifier_remove_ids: Vec::new(),
            modifier_move_ids: Vec::new(),
            add_modifier_button_id: None,
            light_remove_ids: Vec::new(),
            light_name_ids: Vec::new(),
            panel_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            drag_layer_id: None,
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

    /// Node-intent dispatch for this panel's right-click gestures: every
    /// card family's slider resets (main rows + armed drawers). Called from
    /// `UIRoot::repopulate_intents` like every other intent-bearing panel —
    /// the missing hookup was why scene-panel sliders had no right-click
    /// reset while every other slider in the app did.
    pub fn register_intents(&self, intents: &mut crate::intent::IntentRegistry) {
        if !self.open || !matches!(self.state, SceneSetupState::Live(_)) {
            return;
        }
        for card in [
            &self.world_card,
            &self.object_card,
            &self.light_card,
            &self.camera_card,
            &self.modifier_card,
        ] {
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
    /// fresh row values onto the already-built panel without a structural
    /// rebuild. `resolve` maps a row's `RowAddr` to its current value in the
    /// caller's project; `None` skips the row (unresolvable rows keep their
    /// built text). Non-driven rows update their card slider (fill + thumb +
    /// readout); driven rows update their read-only value label through the
    /// handle the driven branch now keeps (`driven_value_ids`).
    pub fn sync_row_values(&self, tree: &mut UITree, resolve: &dyn Fn(&RowAddr) -> Option<f32>) {
        if !self.open {
            return;
        }
        for card in [
            &self.world_card,
            &self.object_card,
            &self.light_card,
            &self.camera_card,
            &self.modifier_card,
        ] {
            for (slot, ids) in card.slider_ids.iter().enumerate() {
                let Some(ids) = ids else { continue };
                let info = &card.param_info[slot];
                let Some((addr, _)) = card.id_map.get(&info.param_id) else { continue };
                let Some(v) = resolve(addr) else { continue };
                let norm = crate::slider::BitmapSlider::value_to_normalized(v, info.min, info.max);
                let text = super::param_slider_shared::format_param_value(
                    v,
                    info.min,
                    info.whole_numbers,
                    info.is_angle,
                    info.value_labels.as_deref(),
                );
                crate::slider::BitmapSlider::update_value(tree, ids, norm, &text);
            }
            for entry in card.driven_value_ids.iter().flatten() {
                let Some(v) = resolve(&entry.addr) else { continue };
                tree.set_text(entry.label, &driven_text(v, &entry.fmt));
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
        // Cleared to 0 rows here (not just resized in `build_world_properties`)
        // so a frame where World never builds (state isn't `Live`) doesn't
        // leave stale ids — `build_world_properties` resizes back to
        // `WORLD_ROW_COUNT` when it does run.
        self.world_card.resize(0);
        // C-P1b: same "cleared here, resized back by whichever build_*
        // branch runs" contract as `world_card` above — `object_card` only
        // ever grows back to `OBJ_ROW_COUNT` when an Object is selected AND
        // its Properties body actually builds.
        self.object_card.resize(0);
        // C-P1c: same contract — `light_card`/`camera_card` only ever grow
        // back to their fixed slot count when their own Properties body
        // actually builds this frame.
        self.light_card.resize(0);
        self.camera_card.resize(0);
        // C-P1d: same contract — `modifier_card` regrows to the selected
        // object's ACTUAL total modifier-param-row count when its
        // properties body builds this frame (variable, unlike the other
        // three families' fixed unions — see `modifier_card`'s doc comment).
        self.modifier_card.resize(0);
        self.add_object_id = None;
        self.add_light_id = None;
        self.import_model_id = None;
        self.outliner_row_ids.clear();
        self.outliner_eye_ids.clear();
        self.object_name_ids.clear();
        self.object_remove_ids.clear();
        self.object_duplicate_ids.clear();
        self.modifier_remove_ids.clear();
        self.modifier_move_ids.clear();
        self.add_modifier_button_id = None;
        self.light_remove_ids.clear();
        self.light_name_ids.clear();
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
    /// REALTIME_3D "Decided — do not reopen" §1).
    fn build_outliner(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
        selected: SceneSelection,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Scene", section_label_style());
        cy += ROW_H;
        cy = self.build_outliner_row(
            tree, inner_x, inner_w, cy, "\u{1F4F7} Camera", SceneSelection::Camera, selected, EyeSlot::Empty,
        );
        cy = self.build_outliner_row(
            tree, inner_x, inner_w, cy, "\u{1F30D} World", SceneSelection::World, selected, EyeSlot::Empty,
        );

        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Lights", section_label_style());
        cy += ROW_H;
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

        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Objects", section_label_style());
        cy += ROW_H;
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
        cy += ROW_GAP;

        // D6: one compact row, three equal-width buttons — was three
        // stacked full-width rows (~200px of permanent height reclaimed).
        let action_w = (inner_w - 2.0 * ROW_GAP) / 3.0;
        self.add_object_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x,
            cy,
            action_w,
            ROW_H,
            btn_style(),
            "+ Object",
            KEY_ADD_OBJECT,
        ));
        self.add_light_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + action_w + ROW_GAP,
            cy,
            action_w,
            ROW_H,
            btn_style(),
            "+ Light",
            KEY_ADD_LIGHT,
        ));
        self.import_model_id = Some(tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + 2.0 * (action_w + ROW_GAP),
            cy,
            action_w,
            ROW_H,
            btn_style(),
            "Import…",
            KEY_IMPORT_MODEL,
        ));
        cy + ROW_H
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

    /// The properties region: a header (name, plus Duplicate/Remove for
    /// Object/Light selections) then the selection's own rows — the EXISTING
    /// curated builders, relocated intact (never a generic param-tree
    /// renderer, v1 D3's named wrong turn).
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
        let btn_w = STEP_W * 3.0;
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
        let dup_id = tree.add_button_keyed(
            Some(self.content_parent),
            inner_x + name_w + 4.0,
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
            inner_x + name_w + 4.0 + STEP_W,
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
    fn build_object_properties_body(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        row: &ObjectKnownRow,
    ) -> f32 {
        let index = row.index;
        // C-P1b: `object_card` regrows to its full fixed slot count every
        // time an Object's Properties body builds — mirrors
        // `build_world_properties`'s `self.world_card.resize(WORLD_ROW_COUNT)`.
        // A slot this object doesn't populate this frame (e.g. `Other`/
        // `None` material skips Metallic/Roughness) simply never gets a
        // `build_object_card_row` call — its `param_info`/`slider_ids` stay
        // at the placeholder `resize` wrote, same "unpopulated slot, never
        // clicked" contract World's `EnvironmentRowVm::None` case already
        // established.
        self.object_card.resize(OBJ_ROW_COUNT);
        if let Some(t) = &row.transform {
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Position X", &t.pos.0, OBJ_ROW_POS_X, "pos_x", false);
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Position Y", &t.pos.1, OBJ_ROW_POS_Y, "pos_y", false);
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Position Z", &t.pos.2, OBJ_ROW_POS_Z, "pos_z", false);
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Rotation X", &t.rot.0, OBJ_ROW_ROT_X, "rot_x", true);
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Rotation Y", &t.rot.1, OBJ_ROW_ROT_Y, "rot_y", true);
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Rotation Z", &t.rot.2, OBJ_ROW_ROT_Z, "rot_z", true);
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Scale X", &t.scale.0, OBJ_ROW_SCALE_X, "scale_x", false);
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Scale Y", &t.scale.1, OBJ_ROW_SCALE_Y, "scale_y", false);
            cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Scale Z", &t.scale.2, OBJ_ROW_SCALE_Z, "scale_z", false);
        }
        match &row.material {
            ObjectMaterialVm::Pbr { color, metallic, roughness } => {
                cy = self.build_object_color_rows(tree, inner_x, inner_w, cy, color);
                cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Metallic", metallic, OBJ_ROW_METALLIC, "metallic", false);
                cy = self.build_object_card_row(tree, inner_x, inner_w, cy, "Roughness", roughness, OBJ_ROW_ROUGHNESS, "roughness", false);
            }
            ObjectMaterialVm::Other { color } => {
                cy = self.build_object_color_rows(tree, inner_x, inner_w, cy, color);
            }
            ObjectMaterialVm::None => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "No material", label_style());
                cy += ROW_H;
            }
        }
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Modifiers", label_style());
        cy += ROW_H;
        if row.modifiers_addable {
            // C-P1d: `modifier_card` regrows to the selected object's
            // ACTUAL total modifier-param-row count this frame (variable,
            // unlike `object_card`'s fixed union above) — see
            // `modifier_card`'s own doc comment for why a running cursor,
            // not a per-family fixed slot table, is the right shape here.
            let total_rows: usize = row.modifiers.iter().map(|m| m.params.len()).sum();
            self.modifier_card.resize(total_rows);
            let mut slot = 0usize;
            for m in &row.modifiers {
                cy = self.build_modifier_row(
                    tree,
                    inner_x,
                    inner_w,
                    cy,
                    index,
                    row.group_node_id.unwrap_or(row.object_node_id),
                    m,
                    row.modifiers.len(),
                    &mut slot,
                );
            }
            cy = self.build_add_modifier_button(
                tree, inner_x, inner_w, cy, index, row.group_node_id.unwrap_or(row.object_node_id),
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
    /// C-P1c: every row now builds through `build_light_card_row` (the
    /// shared card row core) — `self.light_card` regrows to its full fixed
    /// slot count every time a Light's Properties body builds, mirroring
    /// `build_object_properties_body`'s `self.object_card.resize(..)`.
    fn build_light_properties_body(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        row: &LightKnownRow,
    ) -> f32 {
        self.light_card.resize(LIGHT_ROW_COUNT);
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Mode", &row.mode.row, LIGHT_ROW_MODE, "mode",
            Some(&row.mode.labels),
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Color R", &row.color.0, LIGHT_ROW_COLOR_R, "color_r", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Color G", &row.color.1, LIGHT_ROW_COLOR_G, "color_g", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Color B", &row.color.2, LIGHT_ROW_COLOR_B, "color_b", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Intensity", &row.intensity, LIGHT_ROW_INTENSITY, "intensity", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Position X", &row.pos.0, LIGHT_ROW_POS_X, "pos_x", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Position Y", &row.pos.1, LIGHT_ROW_POS_Y, "pos_y", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Position Z", &row.pos.2, LIGHT_ROW_POS_Z, "pos_z", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Aim X", &row.aim.0, LIGHT_ROW_AIM_X, "aim_x", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Aim Y", &row.aim.1, LIGHT_ROW_AIM_Y, "aim_y", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Aim Z", &row.aim.2, LIGHT_ROW_AIM_Z, "aim_z", None,
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Cast Shadows", &row.cast_shadows.row, LIGHT_ROW_CAST_SHADOWS, "cast_shadows",
            Some(&row.cast_shadows.labels),
        );
        cy = self.build_light_card_row(
            tree, inner_x, inner_w, cy, "Shadow Softness", &row.shadow_softness.row, LIGHT_ROW_SHADOW_SOFTNESS,
            "shadow_softness", Some(&row.shadow_softness.labels),
        );
        cy = self.build_light_card_row(
            tree,
            inner_x + PAD,
            inner_w - PAD,
            cy,
            "Light Size",
            &row.light_size,
            LIGHT_ROW_LIGHT_SIZE,
            "light_size",
            None,
        );
        cy + ROW_GAP
    }

    /// World properties: Environment + Fog. C-P1a (SCENE_PANEL_CARD_
    /// CONVERGENCE_DESIGN.md): the four numeric rows (Intensity/Fill/
    /// Density/Height Falloff) now build through the shared card row core
    /// (`build_world_param_row` → `build_param_row`) instead of the deleted
    /// bespoke `build_numeric_row` — same slider chrome, mod glyphs, and
    /// inline drawer every effect/generator card row has. Section chrome
    /// (labels, Mode chip, "+ Add …" buttons) stays panel-shaped, unchanged.
    fn build_world_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
    ) -> f32 {
        self.world_card.resize(WORLD_ROW_COUNT);
        self.world_card.mod_state.audio_send_labels = vm.audio_send_labels.clone();
        self.world_card.mod_state.audio_send_ids = vm.audio_send_ids.clone();

        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Environment", section_label_style());
        cy += ROW_H;
        match &vm.environment {
            EnvironmentRowVm::Importer { mode_is_hdri, intensity, fill, hdri_file } => {
                tree.add_label(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    if *mode_is_hdri { "Mode: HDRI" } else { "Mode: Softbox" },
                    label_style(),
                );
                cy += ROW_H;
                cy = self.build_world_param_row(tree, inner_x, inner_w, cy, "Intensity", intensity, WORLD_ENV_INTENSITY, "intensity");
                cy = self.build_world_param_row(tree, inner_x, inner_w, cy, "Fill", fill, WORLD_ENV_FILL, "fill");
                if !hdri_file.is_empty() {
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
            }
            EnvironmentRowVm::Bare { intensity, fill } => {
                cy = self.build_world_param_row(tree, inner_x, inner_w, cy, "Intensity", intensity, WORLD_ENV_INTENSITY, "intensity");
                cy = self.build_world_param_row(tree, inner_x, inner_w, cy, "Fill", fill, WORLD_ENV_FILL, "fill");
            }
            EnvironmentRowVm::Custom => {
                tree.add_label(
                    Some(self.content_parent),
                    inner_x,
                    cy,
                    inner_w,
                    ROW_H,
                    "Custom (edit in graph)",
                    label_style(),
                );
                cy += ROW_H;
            }
            EnvironmentRowVm::None => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "None", label_style());
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
        }
        cy += ROW_GAP * 2.0;

        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Fog", section_label_style());
        cy += ROW_H;
        match &vm.atmosphere {
            AtmosphereRowVm::Wired { density, height_falloff } => {
                cy = self.build_world_param_row(tree, inner_x, inner_w, cy, "Density", density, WORLD_FOG_DENSITY, "density");
                cy = self.build_world_param_row(
                    tree,
                    inner_x,
                    inner_w,
                    cy,
                    "Height Falloff",
                    height_falloff,
                    WORLD_FOG_HEIGHT_FALLOFF,
                    "height_falloff",
                );
            }
            AtmosphereRowVm::None => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "None", label_style());
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
        }
        cy + ROW_GAP * 2.0
    }

    /// One Environment/Fog row, built through the shared card row core.
    /// `slot` is the row's FIXED index into `self.world_card`
    /// (`WORLD_ENV_INTENSITY`..`WORLD_FOG_HEIGHT_FALLOFF`). `param_key` is
    /// the graph node's own param name (`intensity`/`fill`/`density`/
    /// `height_falloff`) — used ONLY to synthesize this row's stable owned
    /// `ParamId` (D2: `scene.{node_doc_id}.{param_key}`); the actual write
    /// address is `row.value.addr` (unchanged — `RowAddr` already carries
    /// the real `node_doc_id`/`param_id`/`scope_path` `SetGraphNodeParamCommand`
    /// takes).
    #[allow(clippy::too_many_arguments)]
    fn build_world_param_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &ModulatedRow,
        slot: usize,
        param_key: &str,
    ) -> f32 {
        let usable_w = inner_w;

        if row.value.driven {
            // Driven rows stay read-only (D4 of SCENE_PANEL_UX_DESIGN.md) —
            // `build_param_row` has no "driven" concept (card params are
            // never wire-driven), so this one case keeps its own small
            // dimmed render, same presentation the deleted bespoke builder
            // used.
            tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
            let fmt = DrivenFmt::Plain;
            let value_label = tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                usable_w - LABEL_W,
                ROW_H,
                &driven_text(row.value.value, &fmt),
                driven_label_style(),
            );
            self.world_card.slider_ids[slot] = None;
            self.world_card.driven_value_ids[slot] =
                Some(DrivenValueLabel { label: value_label, addr: row.value.addr.clone(), fmt });
            self.world_card.param_info[slot] = placeholder_param_info();
            return cy + ROW_H;
        }

        let param_id = synth_world_param_id(row.value.addr.node_doc_id, param_key);
        let (min, max) = (row.value.min, row.value.max);
        let info = ParamInfo {
            param_id: param_id.clone(),
            name: label.to_string(),
            min,
            max,
            default: row.value.value,
            whole_numbers: false,
            is_angle: false,
            exposed: row.value.exposed,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: None,
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
            section: None,
        };
        self.world_card.param_info[slot] = info.clone();
        self.world_card.id_map.insert(param_id, (row.value.addr.clone(), row.value.value));
        self.sync_world_modulation(slot, &row.modulation);

        let content_w = usable_w;
        let RowGeometry { label_width, slider_w } = super::param_card::row_geometry(content_w, false);
        let built = build_param_row(
            tree,
            Some(self.content_parent),
            inner_x,
            cy,
            slider_w,
            &info,
            &self.world_card.mod_state,
            slot,
            GraphParamTarget::Generator,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            true, // generators always support envelopes — same as ParamCardPanel's generator arm
            label_width,
            self.world_card.mod_active_tab.get(slot).copied().unwrap_or(ModTab::Driver),
            true,
            Some((slot as u64) << 8),
            None,
        );
        self.world_card.row_catcher_ids[slot] = Some(built.row_catcher);
        self.world_card.trim_ids[slot] = built.trim;
        self.world_card.target_ids[slot] = built.target;
        self.world_card.envelope_config_ids[slot] = built.envelope_config;
        self.world_card.envelope_btn_ids[slot] = built.envelope_btn;
        self.world_card.driver_btn_ids[slot] = Some(built.driver_btn);
        self.world_card.driver_config_ids[slot] = built.driver_config;
        self.world_card.audio_btn_ids[slot] = Some(built.audio_btn);
        self.world_card.audio_configs[slot] = built.audio_config;
        self.world_card.mod_tab_ids[slot] = built.mod_tabs;
        self.world_card.slider_ids[slot] = built.slider;
        self.world_card.slider_resets[slot] = Some(built.slider_reset.clone());
        self.world_card.driven_value_ids[slot] = None;
        if let Some(name) = world_row_driver_btn_automation_name(slot) {
            tree.set_name(built.driver_btn, name);
        }
        if let Some(ids) = built.slider {
            self.world_card.drag_sliders[slot].set_ids(ids);
            self.world_card.drag_sliders[slot].set_range(min, max, false);
            if let Some(name) = world_row_automation_name(slot) {
                tree.set_name(ids.value_text, name);
            }
            if let Some(name) = world_row_track_automation_name(slot) {
                tree.set_name(ids.track, name);
            }
        }

        built.new_cy
    }

    /// Copy one row's [`RowModulation`] facts into `self.world_card.mod_state`
    /// at `slot` — the per-row equivalent of `ParamModState::sync_from_config`
    /// and `sync_audio` together, scalar instead of vector-sliced since a
    /// scene row's modulation is computed one row at a time by the app
    /// layer (`row_modulation_for_id`).
    fn sync_world_modulation(&mut self, slot: usize, m: &RowModulation) {
        let ms = &mut self.world_card.mod_state;
        ms.driver_expanded[slot] = m.driver_active;
        ms.envelope_expanded[slot] = m.envelope_active;
        ms.trim_min[slot] = m.trim_min;
        ms.trim_max[slot] = m.trim_max;
        ms.target_norm[slot] = m.target_norm;
        ms.env_decay[slot] = m.env_decay;
        ms.driver_beat_div_idx[slot] = m.driver_beat_div_idx;
        ms.driver_waveform_idx[slot] = m.driver_waveform_idx;
        ms.driver_reversed[slot] = m.driver_reversed;
        ms.driver_dotted[slot] = m.driver_dotted;
        ms.driver_triplet[slot] = m.driver_triplet;
        ms.driver_free_period[slot] = m.driver_free_period;
        ms.automation_active[slot] = m.automation_active;
        ms.automation_overridden[slot] = m.automation_overridden;
        ms.audio_active[slot] = m.audio_active;
        ms.audio_kind_idx[slot] = m.audio_kind_idx;
        ms.audio_band_idx[slot] = m.audio_band_idx;
        ms.audio_range_min[slot] = m.audio_range_min;
        ms.audio_range_max[slot] = m.audio_range_max;
        ms.audio_invert[slot] = m.audio_invert;
        ms.audio_rate[slot] = m.audio_rate;
        ms.audio_sensitivity[slot] = m.audio_sensitivity;
        ms.audio_attack_ms[slot] = m.audio_attack_ms;
        ms.audio_release_ms[slot] = m.audio_release_ms;
        ms.audio_mode_idx[slot] = m.audio_trigger_mode_idx;
        ms.audio_action_idx[slot] = m.audio_action_idx;
        ms.audio_step_amount[slot] = m.audio_step_amount;
        ms.audio_wrap_idx[slot] = m.audio_wrap_idx;
        ms.audio_send_idx[slot] = m
            .audio_send_id
            .as_ref()
            .and_then(|sid| ms.audio_send_ids.iter().position(|s| s == sid))
            .map(|p| p as i32)
            .unwrap_or(-1);
    }

    /// C-P1b (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): one Object row, built
    /// through the shared card row core — the Object-family twin of
    /// `build_world_param_row`, generalized with `is_angle` (World has no
    /// angle rows; Object's Rotation X/Y/Z do) and `mod_offset: Option<u64>`
    /// (Color's per-channel cells never get an exposure button — see
    /// `object_row_mod_offset`'s doc comment). `slot` is the row's FIXED
    /// index into `self.object_card` (`OBJ_ROW_POS_X`..`OBJ_ROW_ROUGHNESS`).
    /// `param_key` is the graph node's own param name, used ONLY to
    /// synthesize this row's stable owned `ParamId` (D2) — the actual write
    /// address is `row.value.addr` (already correctly scoped by
    /// `state_sync`'s `scoped_row`/`transform_row`/`material_row`, D12).
    #[allow(clippy::too_many_arguments)]
    fn build_object_card_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &ModulatedRow,
        slot: usize,
        param_key: &str,
        is_angle: bool,
    ) -> f32 {
        let usable_w = inner_w;

        if row.value.driven {
            tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
            let fmt = if is_angle { DrivenFmt::Degrees } else { DrivenFmt::Plain };
            let value_label = tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                usable_w - LABEL_W,
                ROW_H,
                &driven_text(row.value.value, &fmt),
                driven_label_style(),
            );
            self.object_card.slider_ids[slot] = None;
            self.object_card.driven_value_ids[slot] =
                Some(DrivenValueLabel { label: value_label, addr: row.value.addr.clone(), fmt });
            self.object_card.param_info[slot] = placeholder_param_info();
            return cy + ROW_H;
        }

        let param_id = synth_world_param_id(row.value.addr.node_doc_id, param_key);
        let (min, max) = (row.value.min, row.value.max);
        let info = ParamInfo {
            param_id: param_id.clone(),
            name: label.to_string(),
            min,
            max,
            default: row.value.value,
            whole_numbers: false,
            is_angle,
            exposed: row.value.exposed,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: None,
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
            section: None,
        };
        self.object_card.param_info[slot] = info.clone();
        self.object_card.id_map.insert(param_id, (row.value.addr.clone(), row.value.value));
        self.sync_object_modulation(slot, &row.modulation);

        let content_w = usable_w;
        let RowGeometry { label_width, slider_w } = super::param_card::row_geometry(content_w, false);
        let built = build_param_row(
            tree,
            Some(self.content_parent),
            inner_x,
            cy,
            slider_w,
            &info,
            &self.object_card.mod_state,
            slot,
            GraphParamTarget::Generator,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            true,
            label_width,
            self.object_card.mod_active_tab.get(slot).copied().unwrap_or(ModTab::Driver),
            true,
            Some((slot as u64) << 8),
            None,
        );
        self.object_card.row_catcher_ids[slot] = Some(built.row_catcher);
        self.object_card.trim_ids[slot] = built.trim;
        self.object_card.target_ids[slot] = built.target;
        self.object_card.envelope_config_ids[slot] = built.envelope_config;
        self.object_card.envelope_btn_ids[slot] = built.envelope_btn;
        self.object_card.driver_btn_ids[slot] = Some(built.driver_btn);
        self.object_card.driver_config_ids[slot] = built.driver_config;
        self.object_card.audio_btn_ids[slot] = Some(built.audio_btn);
        self.object_card.audio_configs[slot] = built.audio_config;
        self.object_card.mod_tab_ids[slot] = built.mod_tabs;
        self.object_card.slider_ids[slot] = built.slider;
        self.object_card.slider_resets[slot] = Some(built.slider_reset.clone());
        self.object_card.driven_value_ids[slot] = None;
        if let Some(name) = object_row_driver_btn_automation_name(slot) {
            tree.set_name(built.driver_btn, name);
        }
        if let Some(ids) = built.slider {
            self.object_card.drag_sliders[slot].set_ids(ids);
            self.object_card.drag_sliders[slot].set_range(min, max, false);
            if let Some(name) = object_row_value_automation_name(slot) {
                tree.set_name(ids.value_text, name);
            }
            if let Some(name) = object_row_track_automation_name(slot) {
                tree.set_name(ids.track, name);
            }
        }

        built.new_cy
    }

    /// C-P1b: the Color row — a live square swatch (unchanged from the old
    /// `build_color_row`'s D4 rendering — display-only, no picker) left of
    /// three card rows, one per channel.
    fn build_object_color_rows(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        triplet: &(ModulatedRow, ModulatedRow, ModulatedRow),
    ) -> f32 {
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let swatch_color =
            Color32::new(to_u8(triplet.0.value.value), to_u8(triplet.1.value.value), to_u8(triplet.2.value.value), 255);
        tree.add_panel(
            Some(self.content_parent),
            inner_x,
            cy + (ROW_H - SWATCH_W) * 0.5,
            SWATCH_W,
            SWATCH_W,
            UIStyle {
                bg_color: swatch_color,
                border_color: color::BORDER,
                border_width: 1.0,
                corner_radius: color::SMALL_RADIUS,
                ..UIStyle::default()
            },
        );
        let row_x = inner_x + SWATCH_W + ROW_GAP;
        let row_w = inner_w - SWATCH_W - ROW_GAP;
        let mut cy = self.build_object_card_row(tree, row_x, row_w, cy, "Color R", &triplet.0, OBJ_ROW_COLOR_R, "color_r", false);
        cy = self.build_object_card_row(tree, row_x, row_w, cy, "Color G", &triplet.1, OBJ_ROW_COLOR_G, "color_g", false);
        cy = self.build_object_card_row(tree, row_x, row_w, cy, "Color B", &triplet.2, OBJ_ROW_COLOR_B, "color_b", false);
        cy
    }

    /// C-P1b: copy one Object row's [`RowModulation`] facts into
    /// `self.object_card.mod_state` at `slot` — the Object-family twin of
    /// `sync_world_modulation`.
    fn sync_object_modulation(&mut self, slot: usize, m: &RowModulation) {
        let ms = &mut self.object_card.mod_state;
        ms.driver_expanded[slot] = m.driver_active;
        ms.envelope_expanded[slot] = m.envelope_active;
        ms.trim_min[slot] = m.trim_min;
        ms.trim_max[slot] = m.trim_max;
        ms.target_norm[slot] = m.target_norm;
        ms.env_decay[slot] = m.env_decay;
        ms.driver_beat_div_idx[slot] = m.driver_beat_div_idx;
        ms.driver_waveform_idx[slot] = m.driver_waveform_idx;
        ms.driver_reversed[slot] = m.driver_reversed;
        ms.driver_dotted[slot] = m.driver_dotted;
        ms.driver_triplet[slot] = m.driver_triplet;
        ms.driver_free_period[slot] = m.driver_free_period;
        ms.automation_active[slot] = m.automation_active;
        ms.automation_overridden[slot] = m.automation_overridden;
        ms.audio_active[slot] = m.audio_active;
        ms.audio_kind_idx[slot] = m.audio_kind_idx;
        ms.audio_band_idx[slot] = m.audio_band_idx;
        ms.audio_range_min[slot] = m.audio_range_min;
        ms.audio_range_max[slot] = m.audio_range_max;
        ms.audio_invert[slot] = m.audio_invert;
        ms.audio_rate[slot] = m.audio_rate;
        ms.audio_sensitivity[slot] = m.audio_sensitivity;
        ms.audio_attack_ms[slot] = m.audio_attack_ms;
        ms.audio_release_ms[slot] = m.audio_release_ms;
        ms.audio_mode_idx[slot] = m.audio_trigger_mode_idx;
        ms.audio_action_idx[slot] = m.audio_action_idx;
        ms.audio_step_amount[slot] = m.audio_step_amount;
        ms.audio_wrap_idx[slot] = m.audio_wrap_idx;
        ms.audio_send_idx[slot] = m
            .audio_send_id
            .as_ref()
            .and_then(|sid| ms.audio_send_ids.iter().position(|s| s == sid))
            .map(|p| p as i32)
            .unwrap_or(-1);
    }

    /// C-P1c (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): one Light row, built
    /// through the shared card row core — the Light-family twin of
    /// `build_object_card_row`, generalized with `labels: Option<&[&str]>`
    /// for the enum rows (Mode/Cast Shadows/Shadow Softness): the card row
    /// core already renders a labeled stepper when `ParamInfo.value_labels`
    /// is set (`format_param_value`), so no bespoke enum builder is needed
    /// (D1's "check for a card enum row first and use it if one exists").
    /// `slot` is the row's FIXED index into `self.light_card`
    /// (`LIGHT_ROW_MODE`..`LIGHT_ROW_LIGHT_SIZE`). `index` is the light's own
    /// list index — used only for the mod-button's `light_key` namespace
    /// (unrelated to `slot`, which build_param_row's own `(slot << 8)` keys).
    #[allow(clippy::too_many_arguments)]
    fn build_light_card_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &ModulatedRow,
        slot: usize,
        param_key: &str,
        labels: Option<&[&'static str]>,
    ) -> f32 {
        let usable_w = inner_w;

        if row.value.driven {
            tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
            let fmt = match labels {
                Some(labels) => DrivenFmt::Labels(labels.iter().map(|s| s.to_string()).collect()),
                None => DrivenFmt::Plain,
            };
            let value_label = tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                usable_w - LABEL_W,
                ROW_H,
                &driven_text(row.value.value, &fmt),
                driven_label_style(),
            );
            self.light_card.slider_ids[slot] = None;
            self.light_card.driven_value_ids[slot] =
                Some(DrivenValueLabel { label: value_label, addr: row.value.addr.clone(), fmt });
            self.light_card.param_info[slot] = placeholder_param_info();
            return cy + ROW_H;
        }

        let param_id = synth_world_param_id(row.value.addr.node_doc_id, param_key);
        let (min, max) = (row.value.min, row.value.max);
        let info = ParamInfo {
            param_id: param_id.clone(),
            name: label.to_string(),
            min,
            max,
            default: row.value.value,
            whole_numbers: labels.is_some(),
            is_angle: false,
            exposed: row.value.exposed,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: labels.map(|ls| ls.iter().map(|s| s.to_string()).collect()),
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
            section: None,
        };
        self.light_card.param_info[slot] = info.clone();
        self.light_card.id_map.insert(param_id, (row.value.addr.clone(), row.value.value));
        self.sync_light_modulation(slot, &row.modulation);

        let content_w = usable_w;
        let RowGeometry { label_width, slider_w } = super::param_card::row_geometry(content_w, false);
        let built = build_param_row(
            tree,
            Some(self.content_parent),
            inner_x,
            cy,
            slider_w,
            &info,
            &self.light_card.mod_state,
            slot,
            GraphParamTarget::Generator,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            true,
            label_width,
            self.light_card.mod_active_tab.get(slot).copied().unwrap_or(ModTab::Driver),
            true,
            Some((slot as u64) << 8),
            None,
        );
        self.light_card.row_catcher_ids[slot] = Some(built.row_catcher);
        self.light_card.trim_ids[slot] = built.trim;
        self.light_card.target_ids[slot] = built.target;
        self.light_card.envelope_config_ids[slot] = built.envelope_config;
        self.light_card.envelope_btn_ids[slot] = built.envelope_btn;
        self.light_card.driver_btn_ids[slot] = Some(built.driver_btn);
        self.light_card.driver_config_ids[slot] = built.driver_config;
        self.light_card.audio_btn_ids[slot] = Some(built.audio_btn);
        self.light_card.audio_configs[slot] = built.audio_config;
        self.light_card.mod_tab_ids[slot] = built.mod_tabs;
        self.light_card.slider_ids[slot] = built.slider;
        self.light_card.slider_resets[slot] = Some(built.slider_reset.clone());
        self.light_card.driven_value_ids[slot] = None;
        if let Some(name) = light_row_driver_btn_automation_name(slot) {
            tree.set_name(built.driver_btn, name);
        }
        if let Some(ids) = built.slider {
            self.light_card.drag_sliders[slot].set_ids(ids);
            self.light_card.drag_sliders[slot].set_range(min, max, false);
            if let Some(name) = light_row_value_automation_name(slot) {
                tree.set_name(ids.value_text, name);
            }
            if let Some(name) = light_row_track_automation_name(slot) {
                tree.set_name(ids.track, name);
            }
        }

        built.new_cy
    }

    /// C-P1c: copy one Light row's [`RowModulation`] facts into
    /// `self.light_card.mod_state` at `slot` — the Light-family twin of
    /// `sync_object_modulation`.
    fn sync_light_modulation(&mut self, slot: usize, m: &RowModulation) {
        let ms = &mut self.light_card.mod_state;
        ms.driver_expanded[slot] = m.driver_active;
        ms.envelope_expanded[slot] = m.envelope_active;
        ms.trim_min[slot] = m.trim_min;
        ms.trim_max[slot] = m.trim_max;
        ms.target_norm[slot] = m.target_norm;
        ms.env_decay[slot] = m.env_decay;
        ms.driver_beat_div_idx[slot] = m.driver_beat_div_idx;
        ms.driver_waveform_idx[slot] = m.driver_waveform_idx;
        ms.driver_reversed[slot] = m.driver_reversed;
        ms.driver_dotted[slot] = m.driver_dotted;
        ms.driver_triplet[slot] = m.driver_triplet;
        ms.driver_free_period[slot] = m.driver_free_period;
        ms.automation_active[slot] = m.automation_active;
        ms.automation_overridden[slot] = m.automation_overridden;
        ms.audio_active[slot] = m.audio_active;
        ms.audio_kind_idx[slot] = m.audio_kind_idx;
        ms.audio_band_idx[slot] = m.audio_band_idx;
        ms.audio_range_min[slot] = m.audio_range_min;
        ms.audio_range_max[slot] = m.audio_range_max;
        ms.audio_invert[slot] = m.audio_invert;
        ms.audio_rate[slot] = m.audio_rate;
        ms.audio_sensitivity[slot] = m.audio_sensitivity;
        ms.audio_attack_ms[slot] = m.audio_attack_ms;
        ms.audio_release_ms[slot] = m.audio_release_ms;
        ms.audio_mode_idx[slot] = m.audio_trigger_mode_idx;
        ms.audio_action_idx[slot] = m.audio_action_idx;
        ms.audio_step_amount[slot] = m.audio_step_amount;
        ms.audio_wrap_idx[slot] = m.audio_wrap_idx;
        ms.audio_send_idx[slot] = m
            .audio_send_id
            .as_ref()
            .and_then(|sid| ms.audio_send_ids.iter().position(|s| s == sid))
            .map(|p| p as i32)
            .unwrap_or(-1);
    }

    /// The Camera section (P3, D3/D4): exactly one row set, shape depending
    /// on which camera atom the trace resolved, plus the lens pass-through
    /// row when present. C-P1c: the numeric/triplet rows now build through
    /// `build_camera_card_row` (the shared card row core) instead of the
    /// deleted bespoke steppers — `self.camera_card` regrows to its full
    /// fixed slot count every time the Camera section builds, mirroring
    /// `build_object_properties_body`'s `self.object_card.resize(..)`.
    fn build_camera_section(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32, vm: &SceneSetupVm) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Camera", section_label_style());
        cy += ROW_H;
        self.camera_card.resize(CAM_ROW_COUNT);
        match &vm.camera {
            CameraRowVm::Orbit(row) => {
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Orbit", &row.orbit, CAM_ROW_ORBIT, "orbit");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Tilt", &row.tilt, CAM_ROW_TILT, "tilt");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Distance", &row.distance, CAM_ROW_DISTANCE, "distance");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "FOV", &row.fov_y, CAM_ROW_FOV, "fov_y");
                cy = self.build_camera_lens(tree, inner_x, inner_w, cy, &row.lens);
            }
            CameraRowVm::Free(row) => {
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Position X", &row.pos.0, CAM_ROW_POS_X, "pos_x");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Position Y", &row.pos.1, CAM_ROW_POS_Y, "pos_y");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Position Z", &row.pos.2, CAM_ROW_POS_Z, "pos_z");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Yaw", &row.yaw, CAM_ROW_YAW, "yaw");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Pitch", &row.pitch, CAM_ROW_PITCH, "pitch");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Roll", &row.roll, CAM_ROW_ROLL, "roll");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "FOV", &row.fov_y, CAM_ROW_FOV, "fov_y");
                cy = self.build_camera_lens(tree, inner_x, inner_w, cy, &row.lens);
            }
            CameraRowVm::LookAt(row) => {
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Position X", &row.pos.0, CAM_ROW_POS_X, "pos_x");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Position Y", &row.pos.1, CAM_ROW_POS_Y, "pos_y");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Position Z", &row.pos.2, CAM_ROW_POS_Z, "pos_z");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Target X", &row.target.0, CAM_ROW_TARGET_X, "target_x");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Target Y", &row.target.1, CAM_ROW_TARGET_Y, "target_y");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "Target Z", &row.target.2, CAM_ROW_TARGET_Z, "target_z");
                cy = self.build_camera_card_row(tree, inner_x, inner_w, cy, "FOV", &row.fov_y, CAM_ROW_FOV, "fov_y");
                cy = self.build_camera_lens(tree, inner_x, inner_w, cy, &row.lens);
            }
            CameraRowVm::Custom => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Custom (edit in graph)", label_style());
                cy += ROW_H;
            }
            CameraRowVm::None => {
                // `render_scene`'s `camera` port is REQUIRED (unlike
                // envmap/atmosphere) — every shipped path (importer,
                // Scene Starter) always wires one, so there is no "Add
                // camera" action in v1 (D3).
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "No camera wired", label_style());
                cy += ROW_H;
            }
        }
        cy + ROW_GAP
    }

    fn build_camera_lens(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32, lens: &Option<LensRowVm>) -> f32 {
        let Some(lens) = lens else { return cy };
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Lens", label_style());
        cy += ROW_H;
        let body_x = inner_x + PAD;
        let body_w = inner_w - PAD;
        cy = self.build_camera_card_row(
            tree, body_x, body_w, cy, "Focus Distance", &lens.focus_distance, CAM_ROW_LENS_FOCUS, "focus_distance",
        );
        cy = self.build_camera_card_row(
            tree, body_x, body_w, cy, "F-Stop", &lens.f_stop, CAM_ROW_LENS_FSTOP, "f_stop",
        );
        cy = self.build_camera_card_row(
            tree, body_x, body_w, cy, "Shutter Angle", &lens.shutter_angle, CAM_ROW_LENS_SHUTTER, "shutter_angle",
        );
        self.build_camera_card_row(
            tree, body_x, body_w, cy, "Exposure (EV)", &lens.exposure_ev, CAM_ROW_LENS_EXPOSURE, "exposure_ev",
        )
    }

    /// C-P1c: one Camera row, built through the shared card row core — the
    /// Camera-family twin of `build_object_card_row`/`build_light_card_row`.
    /// `slot` is the row's FIXED index into `self.camera_card`
    /// (`CAM_ROW_ORBIT`..`CAM_ROW_LENS_EXPOSURE`); `is_angle` (D10's degrees
    /// table) is derived structurally from `slot`, not re-derived per call
    /// site.
    fn build_camera_card_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &ModulatedRow,
        slot: usize,
        param_key: &str,
    ) -> f32 {
        let is_angle = camera_row_is_angle(slot);
        let usable_w = inner_w;

        if row.value.driven {
            tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
            let fmt = if is_angle { DrivenFmt::Degrees } else { DrivenFmt::Plain };
            let value_label = tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                usable_w - LABEL_W,
                ROW_H,
                &driven_text(row.value.value, &fmt),
                driven_label_style(),
            );
            self.camera_card.slider_ids[slot] = None;
            self.camera_card.driven_value_ids[slot] =
                Some(DrivenValueLabel { label: value_label, addr: row.value.addr.clone(), fmt });
            self.camera_card.param_info[slot] = placeholder_param_info();
            return cy + ROW_H;
        }

        let param_id = synth_world_param_id(row.value.addr.node_doc_id, param_key);
        let (min, max) = (row.value.min, row.value.max);
        let info = ParamInfo {
            param_id: param_id.clone(),
            name: label.to_string(),
            min,
            max,
            default: row.value.value,
            whole_numbers: false,
            is_angle,
            exposed: row.value.exposed,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: None,
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
            section: None,
        };
        self.camera_card.param_info[slot] = info.clone();
        self.camera_card.id_map.insert(param_id, (row.value.addr.clone(), row.value.value));
        self.sync_camera_modulation(slot, &row.modulation);

        let content_w = usable_w;
        let RowGeometry { label_width, slider_w } = super::param_card::row_geometry(content_w, false);
        let built = build_param_row(
            tree,
            Some(self.content_parent),
            inner_x,
            cy,
            slider_w,
            &info,
            &self.camera_card.mod_state,
            slot,
            GraphParamTarget::Generator,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            true,
            label_width,
            self.camera_card.mod_active_tab.get(slot).copied().unwrap_or(ModTab::Driver),
            true,
            Some((slot as u64) << 8),
            None,
        );
        self.camera_card.row_catcher_ids[slot] = Some(built.row_catcher);
        self.camera_card.trim_ids[slot] = built.trim;
        self.camera_card.target_ids[slot] = built.target;
        self.camera_card.envelope_config_ids[slot] = built.envelope_config;
        self.camera_card.envelope_btn_ids[slot] = built.envelope_btn;
        self.camera_card.driver_btn_ids[slot] = Some(built.driver_btn);
        self.camera_card.driver_config_ids[slot] = built.driver_config;
        self.camera_card.audio_btn_ids[slot] = Some(built.audio_btn);
        self.camera_card.audio_configs[slot] = built.audio_config;
        self.camera_card.mod_tab_ids[slot] = built.mod_tabs;
        self.camera_card.slider_ids[slot] = built.slider;
        self.camera_card.slider_resets[slot] = Some(built.slider_reset.clone());
        self.camera_card.driven_value_ids[slot] = None;
        if let Some(name) = camera_row_driver_btn_automation_name(slot) {
            tree.set_name(built.driver_btn, name);
        }
        if let Some(ids) = built.slider {
            self.camera_card.drag_sliders[slot].set_ids(ids);
            self.camera_card.drag_sliders[slot].set_range(min, max, false);
            if let Some(name) = camera_row_value_automation_name(slot) {
                tree.set_name(ids.value_text, name);
            }
            if let Some(name) = camera_row_track_automation_name(slot) {
                tree.set_name(ids.track, name);
            }
        }

        built.new_cy
    }

    /// C-P1c: copy one Camera row's [`RowModulation`] facts into
    /// `self.camera_card.mod_state` at `slot` — the Camera-family twin of
    /// `sync_object_modulation`/`sync_light_modulation`.
    fn sync_camera_modulation(&mut self, slot: usize, m: &RowModulation) {
        let ms = &mut self.camera_card.mod_state;
        ms.driver_expanded[slot] = m.driver_active;
        ms.envelope_expanded[slot] = m.envelope_active;
        ms.trim_min[slot] = m.trim_min;
        ms.trim_max[slot] = m.trim_max;
        ms.target_norm[slot] = m.target_norm;
        ms.env_decay[slot] = m.env_decay;
        ms.driver_beat_div_idx[slot] = m.driver_beat_div_idx;
        ms.driver_waveform_idx[slot] = m.driver_waveform_idx;
        ms.driver_reversed[slot] = m.driver_reversed;
        ms.driver_dotted[slot] = m.driver_dotted;
        ms.driver_triplet[slot] = m.driver_triplet;
        ms.driver_free_period[slot] = m.driver_free_period;
        ms.automation_active[slot] = m.automation_active;
        ms.automation_overridden[slot] = m.automation_overridden;
        ms.audio_active[slot] = m.audio_active;
        ms.audio_kind_idx[slot] = m.audio_kind_idx;
        ms.audio_band_idx[slot] = m.audio_band_idx;
        ms.audio_range_min[slot] = m.audio_range_min;
        ms.audio_range_max[slot] = m.audio_range_max;
        ms.audio_invert[slot] = m.audio_invert;
        ms.audio_rate[slot] = m.audio_rate;
        ms.audio_sensitivity[slot] = m.audio_sensitivity;
        ms.audio_attack_ms[slot] = m.audio_attack_ms;
        ms.audio_release_ms[slot] = m.audio_release_ms;
        ms.audio_mode_idx[slot] = m.audio_trigger_mode_idx;
        ms.audio_action_idx[slot] = m.audio_action_idx;
        ms.audio_step_amount[slot] = m.audio_step_amount;
        ms.audio_wrap_idx[slot] = m.audio_wrap_idx;
        ms.audio_send_idx[slot] = m
            .audio_send_id
            .as_ref()
            .and_then(|sid| ms.audio_send_ids.iter().position(|s| s == sid))
            .map(|p| p as i32)
            .unwrap_or(-1);
    }

    /// One modifier-stack entry (P5/D6): display name + up/down/remove
    /// (panel chrome — stays bespoke, D1's own carve-out for "rows that
    /// aren't params"), then its own param rows built through the shared
    /// card row core (C-P1d). `mod_count` is the CURRENT stack length —
    /// up/down are always rendered (never conditionally hidden,
    /// `feedback_no_conditionally_visible_ui`) but only recorded as live
    /// targets when they wouldn't push past a stack boundary; clicking an
    /// inert one at the boundary is simply a no-op. `slot` is a running
    /// cursor into `self.modifier_card` — the caller
    /// (`build_object_properties_body`) pre-sizes `modifier_card` to the
    /// selected object's TOTAL modifier-param-row count for this frame and
    /// threads the same cursor across every modifier in the stack, so each
    /// param row gets its own stable-for-this-frame array slot regardless
    /// of which modifier it belongs to.
    #[allow(clippy::too_many_arguments)]
    fn build_modifier_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        object_index: usize,
        group_node_id: u32,
        m: &ModifierKnownRow,
        mod_count: usize,
        slot: &mut usize,
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
        cy += ROW_H;

        let param_x = inner_x + PAD;
        let param_w = inner_w - PAD;
        for (param_slot, p) in m.params.iter().enumerate() {
            let this_slot = *slot;
            *slot += 1;
            cy = match p {
                // UX-P3b-i: the mod-button's `param_label` disambiguates
                // WHICH modifier's field this is (an object can carry
                // several modifiers, each with its own "Angle"/"Amount") —
                // "Bend Angle", not a bare "Angle" that would collide with
                // a Twist modifier's own numeric row on the same object.
                ModifierParamRowVm::Numeric { label, row } => {
                    let param_key = row.value.addr.param_id.clone();
                    self.build_modifier_card_row(
                        tree, param_x, param_w, cy, label, row, this_slot, &param_key, param_slot, None,
                    )
                }
                ModifierParamRowVm::Axis { label, row } => {
                    let param_key = row.row.value.addr.param_id.clone();
                    self.build_modifier_card_row(
                        tree, param_x, param_w, cy, label, &row.row, this_slot, &param_key, param_slot, Some(&row.labels),
                    )
                }
            };
        }
        cy
    }

    /// C-P1d (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): one Modifier param
    /// row, built through the shared card row core — the Modifier-family
    /// twin of `build_light_card_row` (Axis rows ride `labels` the SAME
    /// `value_labels` path Light's Mode/Cast Shadows/Shadow Softness rows
    /// already proved, D1's "check for a card enum row first"). Unlike the
    /// other three families, `slot` is NOT a fixed per-family index — it's
    /// the running cursor `build_modifier_row` threads across the whole
    /// stack (see `modifier_card`'s own doc comment). `param_slot` (0-based,
    /// WITHIN this one modifier's own param list) is used only for stable
    /// automation names (`modifier_param_*_automation_name`) — reused
    /// across every modifier instance via `nth` in `scripts/ui-flows/`, the
    /// same "name over raw pixel coordinate" convention every fixed-slot
    /// family already follows, just keyed one level more locally since the
    /// stack itself has no fixed width. Only `Numeric` rows get a mod
    /// button (`labels.is_none()`) — `Axis` rows stay unexposable, a
    /// structural switch, same reasoning as Light's Mode row.
    #[allow(clippy::too_many_arguments)]
    fn build_modifier_card_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &ModulatedRow,
        slot: usize,
        param_key: &str,
        param_slot: usize,
        labels: Option<&[&'static str]>,
    ) -> f32 {
        let usable_w = inner_w;

        if row.value.driven {
            tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
            let fmt = match labels {
                Some(labels) => DrivenFmt::Labels(labels.iter().map(|s| s.to_string()).collect()),
                None => DrivenFmt::Plain,
            };
            let value_label = tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                usable_w - LABEL_W,
                ROW_H,
                &driven_text(row.value.value, &fmt),
                driven_label_style(),
            );
            self.modifier_card.slider_ids[slot] = None;
            self.modifier_card.driven_value_ids[slot] =
                Some(DrivenValueLabel { label: value_label, addr: row.value.addr.clone(), fmt });
            self.modifier_card.param_info[slot] = placeholder_param_info();
            return cy + ROW_H;
        }

        let param_id = synth_world_param_id(row.value.addr.node_doc_id, param_key);
        let (min, max) = (row.value.min, row.value.max);
        let info = ParamInfo {
            param_id: param_id.clone(),
            name: label.to_string(),
            min,
            max,
            default: row.value.value,
            whole_numbers: labels.is_some(),
            is_angle: false,
            exposed: row.value.exposed,
            is_toggle: false,
            is_trigger: false,
            is_trigger_gate: false,
            value_labels: labels.map(|ls| ls.iter().map(|s| s.to_string()).collect()),
            osc_address: None,
            ableton_display: None,
            ableton_range: None,
            mappable: false,
            section: None,
        };
        self.modifier_card.param_info[slot] = info.clone();
        self.modifier_card.id_map.insert(param_id, (row.value.addr.clone(), row.value.value));
        self.sync_modifier_modulation(slot, &row.modulation);

        let content_w = usable_w;
        let RowGeometry { label_width, slider_w } = super::param_card::row_geometry(content_w, false);
        let built = build_param_row(
            tree,
            Some(self.content_parent),
            inner_x,
            cy,
            slider_w,
            &info,
            &self.modifier_card.mod_state,
            slot,
            GraphParamTarget::Generator,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            true,
            label_width,
            self.modifier_card.mod_active_tab.get(slot).copied().unwrap_or(ModTab::Driver),
            true,
            // C-P1d: unlike World/Object/Light/Camera (mutually exclusive
            // Properties-body sections — never two of them build under the
            // same `content_parent` in one frame), Modifier rows ALWAYS
            // build alongside `object_card`'s own rows (the stack lives
            // INSIDE the selected object's properties body) — so a bare
            // `(slot << 8)` here would collide with `object_card`'s own
            // slot-0..13 key range as sibling WidgetIds under the same
            // parent. `MODIFIER_CARD_ROW_KEY_OFFSET` pushes this family's
            // whole key range far clear of every other family's.
            Some((MODIFIER_CARD_ROW_KEY_OFFSET + slot as u64) << 8),
            None,
        );
        self.modifier_card.row_catcher_ids[slot] = Some(built.row_catcher);
        self.modifier_card.trim_ids[slot] = built.trim;
        self.modifier_card.target_ids[slot] = built.target;
        self.modifier_card.envelope_config_ids[slot] = built.envelope_config;
        self.modifier_card.envelope_btn_ids[slot] = built.envelope_btn;
        self.modifier_card.driver_btn_ids[slot] = Some(built.driver_btn);
        self.modifier_card.driver_config_ids[slot] = built.driver_config;
        self.modifier_card.audio_btn_ids[slot] = Some(built.audio_btn);
        self.modifier_card.audio_configs[slot] = built.audio_config;
        self.modifier_card.mod_tab_ids[slot] = built.mod_tabs;
        self.modifier_card.slider_ids[slot] = built.slider;
        self.modifier_card.slider_resets[slot] = Some(built.slider_reset.clone());
        self.modifier_card.driven_value_ids[slot] = None;
        if let Some(name) = modifier_param_driver_btn_automation_name(param_slot) {
            tree.set_name(built.driver_btn, name);
        }
        if let Some(ids) = built.slider {
            self.modifier_card.drag_sliders[slot].set_ids(ids);
            self.modifier_card.drag_sliders[slot].set_range(min, max, false);
            if let Some(name) = modifier_param_row_automation_name(param_slot) {
                tree.set_name(ids.value_text, name);
            }
            if let Some(name) = modifier_param_track_automation_name(param_slot) {
                tree.set_name(ids.track, name);
            }
        }

        built.new_cy
    }

    /// C-P1d: copy one Modifier row's [`RowModulation`] facts into
    /// `self.modifier_card.mod_state` at `slot` — the Modifier-family twin
    /// of `sync_light_modulation`.
    fn sync_modifier_modulation(&mut self, slot: usize, m: &RowModulation) {
        let ms = &mut self.modifier_card.mod_state;
        ms.driver_expanded[slot] = m.driver_active;
        ms.envelope_expanded[slot] = m.envelope_active;
        ms.trim_min[slot] = m.trim_min;
        ms.trim_max[slot] = m.trim_max;
        ms.target_norm[slot] = m.target_norm;
        ms.env_decay[slot] = m.env_decay;
        ms.driver_beat_div_idx[slot] = m.driver_beat_div_idx;
        ms.driver_waveform_idx[slot] = m.driver_waveform_idx;
        ms.driver_reversed[slot] = m.driver_reversed;
        ms.driver_dotted[slot] = m.driver_dotted;
        ms.driver_triplet[slot] = m.driver_triplet;
        ms.driver_free_period[slot] = m.driver_free_period;
        ms.automation_active[slot] = m.automation_active;
        ms.automation_overridden[slot] = m.automation_overridden;
        ms.audio_active[slot] = m.audio_active;
        ms.audio_kind_idx[slot] = m.audio_kind_idx;
        ms.audio_band_idx[slot] = m.audio_band_idx;
        ms.audio_range_min[slot] = m.audio_range_min;
        ms.audio_range_max[slot] = m.audio_range_max;
        ms.audio_invert[slot] = m.audio_invert;
        ms.audio_rate[slot] = m.audio_rate;
        ms.audio_sensitivity[slot] = m.audio_sensitivity;
        ms.audio_attack_ms[slot] = m.audio_attack_ms;
        ms.audio_release_ms[slot] = m.audio_release_ms;
        ms.audio_mode_idx[slot] = m.audio_trigger_mode_idx;
        ms.audio_action_idx[slot] = m.audio_action_idx;
        ms.audio_step_amount[slot] = m.audio_step_amount;
        ms.audio_wrap_idx[slot] = m.audio_wrap_idx;
        ms.audio_send_idx[slot] = m
            .audio_send_id
            .as_ref()
            .and_then(|sid| ms.audio_send_ids.iter().position(|s| s == sid))
            .map(|p| p as i32)
            .unwrap_or(-1);
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
    pub fn handle_event(&mut self, event: &UIEvent) -> (bool, Vec<PanelAction>) {
        if !self.open {
            return (false, Vec::new());
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
                    return (true, vec![PanelAction::OpenSceneSetup]);
                }
                // D7: an outliner row click sets the UI-local selection —
                // no command, no undo unit, valid even before a `Live` state
                // exists. D1 of SCENE_PANEL_UX_DESIGN.md: also emit
                // `SceneSetupSelectionChanged` so the dispatch loop's
                // `structural_change: true` rebuilds Properties THIS frame
                // instead of waiting for the next unrelated sync.
                if let Some((_, sel)) = self.outliner_row_ids.iter().find(|(id, _)| *id == *node_id) {
                    if let SceneSetupState::Live(vm) = &self.state {
                        self.selection.insert(vm.layer_id.clone(), *sel);
                        return (true, vec![PanelAction::SceneSetupSelectionChanged(vm.layer_id.clone())]);
                    }
                    return (true, Vec::new());
                }
                let mut actions = Vec::new();
                if let SceneSetupState::Live(vm) = &self.state {
                    if let Some((_, row_value)) =
                        self.outliner_eye_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        // The eye toggle: writes `scene_object.visible`
                        // through the SAME fourth-surface path every other
                        // row uses — the [0,1] threshold flips between 0.0
                        // and 1.0 (D3's on/off convention).
                        let new_value = if row_value.value > 0.5 { 0.0 } else { 1.0 };
                        actions.push(PanelAction::SceneSetupParamChanged(
                            vm.layer_id.clone(),
                            row_value.addr.scope_path.clone(),
                            row_value.addr.node_doc_id,
                            row_value.addr.param_id.clone(),
                            new_value,
                        ));
                    } else if let Some((_, index)) =
                        self.object_duplicate_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::SceneSetupDuplicateObject(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            *index as u32,
                        ));
                    } else if let Some((light_node_id, _, current_name)) =
                        self.light_name_ids.iter().find(|(_, id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::SceneSetupRenameLightClicked(
                            vm.layer_id.clone(),
                            *light_node_id,
                            current_name.clone(),
                        ));
                    }
                }
                if let SceneSetupState::Live(vm) = &self.state {
                    if self.add_environment_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupAddEnvironment(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                        ));
                    } else if self.add_fog_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupAddFog(vm.layer_id.clone(), vm.scene_root_node_id));
                    } else if self.add_object_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupAddObject(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            vm.object_count as u32,
                        ));
                    } else if self.add_light_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupAddLight(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            vm.light_count as u32,
                        ));
                    } else if self.import_model_id == Some(*node_id) {
                        actions.push(PanelAction::SceneSetupImportModelClicked(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                        ));
                    } else if let Some((group_node_id, _, current_name)) =
                        self.object_name_ids.iter().find(|(_, id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::SceneSetupRenameObjectClicked(
                            vm.layer_id.clone(),
                            *group_node_id,
                            current_name.clone(),
                        ));
                    } else if let Some((_, group_node_id)) =
                        self.add_modifier_button_id.filter(|(id, _)| *id == *node_id)
                    {
                        // UX-P2 (D6): the button doesn't resolve a choice
                        // itself — it asks the app to open the shared
                        // dropdown (`SceneSetupAddModifierClicked`), which
                        // lists `MESH_MODIFIER_CHOICES` and dispatches the
                        // SAME `SceneSetupAddModifier` each old chip did.
                        actions.push(PanelAction::SceneSetupAddModifierClicked(
                            vm.layer_id.clone(),
                            group_node_id,
                            *node_id,
                        ));
                    } else if let Some((_, group_node_id, modifier_node_id)) =
                        self.modifier_remove_ids.iter().find(|(id, _, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::SceneSetupRemoveModifier(
                            vm.layer_id.clone(),
                            *group_node_id,
                            *modifier_node_id,
                        ));
                    } else if let Some((_, group_node_id, modifier_node_id, new_position)) =
                        self.modifier_move_ids.iter().find(|(id, _, _, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::SceneSetupMoveModifier(
                            vm.layer_id.clone(),
                            *group_node_id,
                            *modifier_node_id,
                            *new_position,
                        ));
                    } else if let Some((_, index)) =
                        self.object_remove_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::SceneSetupRemoveObject(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            *index as u32,
                        ));
                    } else if let Some((_, index)) =
                        self.light_remove_ids.iter().find(|(id, _)| *id == *node_id)
                    {
                        actions.push(PanelAction::SceneSetupRemoveLight(
                            vm.layer_id.clone(),
                            vm.scene_root_node_id,
                            *index as u32,
                        ));
                    } else if let Some(rc) = match_param_row_click(
                        *node_id,
                        &self.world_card.driver_btn_ids,
                        &self.world_card.envelope_btn_ids,
                        &self.world_card.driver_config_ids,
                        &self.world_card.ableton_config_ids,
                        &self.world_card.audio_btn_ids,
                        &self.world_card.audio_configs,
                        &self.world_card.slider_ids,
                        &self.world_card.osc_addresses,
                        &self.world_card.param_info,
                        &self.world_card.mod_state,
                    ) {
                        // C-P1a: the converted Environment/Fog family's D/E/A
                        // buttons + config drawers — the SAME dispatch shape
                        // `ParamCardPanel::handle_click_generator` uses,
                        // targeting `GraphParamTarget::Generator` (a scene
                        // row always lives on the layer's own generator).
                        let target = GraphParamTarget::Generator;
                        actions.extend(match rc {
                            RowClick::DriverToggle(pi) => {
                                self.world_card.focus_mod_tab(pi, ModTab::Driver);
                                vec![PanelAction::DriverToggle(target, self.world_card.pid_at(pi))]
                            }
                            RowClick::EnvelopeToggle(pi) => {
                                self.world_card.focus_mod_tab(pi, ModTab::Envelope);
                                vec![PanelAction::EnvelopeToggle(target, self.world_card.pid_at(pi))]
                            }
                            RowClick::DriverConfig(pi, action) => {
                                vec![PanelAction::DriverConfig(target, self.world_card.pid_at(pi), action)]
                            }
                            RowClick::AbletonInvert(pi) => {
                                vec![PanelAction::AbletonInvertToggle(target, self.world_card.pid_at(pi))]
                            }
                            RowClick::AudioToggle(pi) => {
                                self.world_card.focus_mod_tab(pi, ModTab::Audio);
                                self.world_card.audio_toggle_action(target, pi)
                            }
                            RowClick::AudioSelectSend(pi, k) => {
                                self.world_card.audio_set_source_action(target, pi, Some(k), None, None)
                            }
                            RowClick::AudioSelectKind(pi, k) => {
                                self.world_card.audio_set_source_action(target, pi, None, Some(k), None)
                            }
                            RowClick::AudioSelectBand(pi, b) => {
                                self.world_card.audio_set_source_action(target, pi, None, None, Some(b))
                            }
                            RowClick::AudioToggleInvert(pi) => {
                                vec![PanelAction::AudioModSetInvert(target, self.world_card.pid_at(pi))]
                            }
                            RowClick::AudioSelectTriggerMode(pi, m) => {
                                vec![PanelAction::AudioModSetTriggerMode(target, self.world_card.pid_at(pi), m)]
                            }
                            RowClick::AudioSelectAction(pi, k) => {
                                vec![PanelAction::AudioModSetActionKind(target, self.world_card.pid_at(pi), k)]
                            }
                            RowClick::AudioSelectWrap(pi, w) => {
                                vec![PanelAction::AudioModSetWrap(target, self.world_card.pid_at(pi), w)]
                            }
                            RowClick::LabelCopy(_) => Vec::new(),
                            RowClick::EnumValueCell(pi) => {
                                self.world_card.enum_value_cell_action(pi, *node_id)
                            }
                        });
                    } else if let Some((pi, tab)) = self.world_card.mod_tab_hit(*node_id) {
                        self.world_card.focus_mod_tab(pi, tab);
                        actions.push(PanelAction::ModConfigTabChanged);
                    } else if let Some(rc) = match_param_row_click(
                        *node_id,
                        &self.object_card.driver_btn_ids,
                        &self.object_card.envelope_btn_ids,
                        &self.object_card.driver_config_ids,
                        &self.object_card.ableton_config_ids,
                        &self.object_card.audio_btn_ids,
                        &self.object_card.audio_configs,
                        &self.object_card.slider_ids,
                        &self.object_card.osc_addresses,
                        &self.object_card.param_info,
                        &self.object_card.mod_state,
                    ) {
                        // C-P1b: the converted Object family's D/E/A buttons
                        // + config drawers — same dispatch shape as
                        // `world_card`'s branch above, generalized.
                        let target = GraphParamTarget::Generator;
                        actions.extend(match rc {
                            RowClick::DriverToggle(pi) => {
                                self.object_card.focus_mod_tab(pi, ModTab::Driver);
                                vec![PanelAction::DriverToggle(target, self.object_card.pid_at(pi))]
                            }
                            RowClick::EnvelopeToggle(pi) => {
                                self.object_card.focus_mod_tab(pi, ModTab::Envelope);
                                vec![PanelAction::EnvelopeToggle(target, self.object_card.pid_at(pi))]
                            }
                            RowClick::DriverConfig(pi, action) => {
                                vec![PanelAction::DriverConfig(target, self.object_card.pid_at(pi), action)]
                            }
                            RowClick::AbletonInvert(pi) => {
                                vec![PanelAction::AbletonInvertToggle(target, self.object_card.pid_at(pi))]
                            }
                            RowClick::AudioToggle(pi) => {
                                self.object_card.focus_mod_tab(pi, ModTab::Audio);
                                self.object_card.audio_toggle_action(target, pi)
                            }
                            RowClick::AudioSelectSend(pi, k) => {
                                self.object_card.audio_set_source_action(target, pi, Some(k), None, None)
                            }
                            RowClick::AudioSelectKind(pi, k) => {
                                self.object_card.audio_set_source_action(target, pi, None, Some(k), None)
                            }
                            RowClick::AudioSelectBand(pi, b) => {
                                self.object_card.audio_set_source_action(target, pi, None, None, Some(b))
                            }
                            RowClick::AudioToggleInvert(pi) => {
                                vec![PanelAction::AudioModSetInvert(target, self.object_card.pid_at(pi))]
                            }
                            RowClick::AudioSelectTriggerMode(pi, m) => {
                                vec![PanelAction::AudioModSetTriggerMode(target, self.object_card.pid_at(pi), m)]
                            }
                            RowClick::AudioSelectAction(pi, k) => {
                                vec![PanelAction::AudioModSetActionKind(target, self.object_card.pid_at(pi), k)]
                            }
                            RowClick::AudioSelectWrap(pi, w) => {
                                vec![PanelAction::AudioModSetWrap(target, self.object_card.pid_at(pi), w)]
                            }
                            RowClick::LabelCopy(_) => Vec::new(),
                            RowClick::EnumValueCell(pi) => {
                                self.object_card.enum_value_cell_action(pi, *node_id)
                            }
                        });
                    } else if let Some((pi, tab)) = self.object_card.mod_tab_hit(*node_id) {
                        self.object_card.focus_mod_tab(pi, tab);
                        actions.push(PanelAction::ModConfigTabChanged);
                    } else if let Some(rc) = match_param_row_click(
                        *node_id,
                        &self.light_card.driver_btn_ids,
                        &self.light_card.envelope_btn_ids,
                        &self.light_card.driver_config_ids,
                        &self.light_card.ableton_config_ids,
                        &self.light_card.audio_btn_ids,
                        &self.light_card.audio_configs,
                        &self.light_card.slider_ids,
                        &self.light_card.osc_addresses,
                        &self.light_card.param_info,
                        &self.light_card.mod_state,
                    ) {
                        // C-P1c: the converted Light family's D/E/A buttons +
                        // config drawers — same dispatch shape as
                        // `object_card`'s branch above, generalized.
                        let target = GraphParamTarget::Generator;
                        actions.extend(match rc {
                            RowClick::DriverToggle(pi) => {
                                self.light_card.focus_mod_tab(pi, ModTab::Driver);
                                vec![PanelAction::DriverToggle(target, self.light_card.pid_at(pi))]
                            }
                            RowClick::EnvelopeToggle(pi) => {
                                self.light_card.focus_mod_tab(pi, ModTab::Envelope);
                                vec![PanelAction::EnvelopeToggle(target, self.light_card.pid_at(pi))]
                            }
                            RowClick::DriverConfig(pi, action) => {
                                vec![PanelAction::DriverConfig(target, self.light_card.pid_at(pi), action)]
                            }
                            RowClick::AbletonInvert(pi) => {
                                vec![PanelAction::AbletonInvertToggle(target, self.light_card.pid_at(pi))]
                            }
                            RowClick::AudioToggle(pi) => {
                                self.light_card.focus_mod_tab(pi, ModTab::Audio);
                                self.light_card.audio_toggle_action(target, pi)
                            }
                            RowClick::AudioSelectSend(pi, k) => {
                                self.light_card.audio_set_source_action(target, pi, Some(k), None, None)
                            }
                            RowClick::AudioSelectKind(pi, k) => {
                                self.light_card.audio_set_source_action(target, pi, None, Some(k), None)
                            }
                            RowClick::AudioSelectBand(pi, b) => {
                                self.light_card.audio_set_source_action(target, pi, None, None, Some(b))
                            }
                            RowClick::AudioToggleInvert(pi) => {
                                vec![PanelAction::AudioModSetInvert(target, self.light_card.pid_at(pi))]
                            }
                            RowClick::AudioSelectTriggerMode(pi, m) => {
                                vec![PanelAction::AudioModSetTriggerMode(target, self.light_card.pid_at(pi), m)]
                            }
                            RowClick::AudioSelectAction(pi, k) => {
                                vec![PanelAction::AudioModSetActionKind(target, self.light_card.pid_at(pi), k)]
                            }
                            RowClick::AudioSelectWrap(pi, w) => {
                                vec![PanelAction::AudioModSetWrap(target, self.light_card.pid_at(pi), w)]
                            }
                            RowClick::LabelCopy(_) => Vec::new(),
                            RowClick::EnumValueCell(pi) => {
                                self.light_card.enum_value_cell_action(pi, *node_id)
                            }
                        });
                    } else if let Some((pi, tab)) = self.light_card.mod_tab_hit(*node_id) {
                        self.light_card.focus_mod_tab(pi, tab);
                        actions.push(PanelAction::ModConfigTabChanged);
                    } else if let Some(rc) = match_param_row_click(
                        *node_id,
                        &self.camera_card.driver_btn_ids,
                        &self.camera_card.envelope_btn_ids,
                        &self.camera_card.driver_config_ids,
                        &self.camera_card.ableton_config_ids,
                        &self.camera_card.audio_btn_ids,
                        &self.camera_card.audio_configs,
                        &self.camera_card.slider_ids,
                        &self.camera_card.osc_addresses,
                        &self.camera_card.param_info,
                        &self.camera_card.mod_state,
                    ) {
                        // C-P1c: the converted Camera family's D/E/A buttons
                        // + config drawers — same dispatch shape as
                        // `light_card`'s branch above, generalized.
                        let target = GraphParamTarget::Generator;
                        actions.extend(match rc {
                            RowClick::DriverToggle(pi) => {
                                self.camera_card.focus_mod_tab(pi, ModTab::Driver);
                                vec![PanelAction::DriverToggle(target, self.camera_card.pid_at(pi))]
                            }
                            RowClick::EnvelopeToggle(pi) => {
                                self.camera_card.focus_mod_tab(pi, ModTab::Envelope);
                                vec![PanelAction::EnvelopeToggle(target, self.camera_card.pid_at(pi))]
                            }
                            RowClick::DriverConfig(pi, action) => {
                                vec![PanelAction::DriverConfig(target, self.camera_card.pid_at(pi), action)]
                            }
                            RowClick::AbletonInvert(pi) => {
                                vec![PanelAction::AbletonInvertToggle(target, self.camera_card.pid_at(pi))]
                            }
                            RowClick::AudioToggle(pi) => {
                                self.camera_card.focus_mod_tab(pi, ModTab::Audio);
                                self.camera_card.audio_toggle_action(target, pi)
                            }
                            RowClick::AudioSelectSend(pi, k) => {
                                self.camera_card.audio_set_source_action(target, pi, Some(k), None, None)
                            }
                            RowClick::AudioSelectKind(pi, k) => {
                                self.camera_card.audio_set_source_action(target, pi, None, Some(k), None)
                            }
                            RowClick::AudioSelectBand(pi, b) => {
                                self.camera_card.audio_set_source_action(target, pi, None, None, Some(b))
                            }
                            RowClick::AudioToggleInvert(pi) => {
                                vec![PanelAction::AudioModSetInvert(target, self.camera_card.pid_at(pi))]
                            }
                            RowClick::AudioSelectTriggerMode(pi, m) => {
                                vec![PanelAction::AudioModSetTriggerMode(target, self.camera_card.pid_at(pi), m)]
                            }
                            RowClick::AudioSelectAction(pi, k) => {
                                vec![PanelAction::AudioModSetActionKind(target, self.camera_card.pid_at(pi), k)]
                            }
                            RowClick::AudioSelectWrap(pi, w) => {
                                vec![PanelAction::AudioModSetWrap(target, self.camera_card.pid_at(pi), w)]
                            }
                            RowClick::LabelCopy(_) => Vec::new(),
                            RowClick::EnumValueCell(pi) => {
                                self.camera_card.enum_value_cell_action(pi, *node_id)
                            }
                        });
                    } else if let Some((pi, tab)) = self.camera_card.mod_tab_hit(*node_id) {
                        self.camera_card.focus_mod_tab(pi, tab);
                        actions.push(PanelAction::ModConfigTabChanged);
                    } else if let Some(rc) = match_param_row_click(
                        *node_id,
                        &self.modifier_card.driver_btn_ids,
                        &self.modifier_card.envelope_btn_ids,
                        &self.modifier_card.driver_config_ids,
                        &self.modifier_card.ableton_config_ids,
                        &self.modifier_card.audio_btn_ids,
                        &self.modifier_card.audio_configs,
                        &self.modifier_card.slider_ids,
                        &self.modifier_card.osc_addresses,
                        &self.modifier_card.param_info,
                        &self.modifier_card.mod_state,
                    ) {
                        // C-P1d: the converted Modifier family's D/E/A
                        // buttons + config drawers — same dispatch shape as
                        // `camera_card`'s branch above, generalized.
                        let target = GraphParamTarget::Generator;
                        actions.extend(match rc {
                            RowClick::DriverToggle(pi) => {
                                self.modifier_card.focus_mod_tab(pi, ModTab::Driver);
                                vec![PanelAction::DriverToggle(target, self.modifier_card.pid_at(pi))]
                            }
                            RowClick::EnvelopeToggle(pi) => {
                                self.modifier_card.focus_mod_tab(pi, ModTab::Envelope);
                                vec![PanelAction::EnvelopeToggle(target, self.modifier_card.pid_at(pi))]
                            }
                            RowClick::DriverConfig(pi, action) => {
                                vec![PanelAction::DriverConfig(target, self.modifier_card.pid_at(pi), action)]
                            }
                            RowClick::AbletonInvert(pi) => {
                                vec![PanelAction::AbletonInvertToggle(target, self.modifier_card.pid_at(pi))]
                            }
                            RowClick::AudioToggle(pi) => {
                                self.modifier_card.focus_mod_tab(pi, ModTab::Audio);
                                self.modifier_card.audio_toggle_action(target, pi)
                            }
                            RowClick::AudioSelectSend(pi, k) => {
                                self.modifier_card.audio_set_source_action(target, pi, Some(k), None, None)
                            }
                            RowClick::AudioSelectKind(pi, k) => {
                                self.modifier_card.audio_set_source_action(target, pi, None, Some(k), None)
                            }
                            RowClick::AudioSelectBand(pi, b) => {
                                self.modifier_card.audio_set_source_action(target, pi, None, None, Some(b))
                            }
                            RowClick::AudioToggleInvert(pi) => {
                                vec![PanelAction::AudioModSetInvert(target, self.modifier_card.pid_at(pi))]
                            }
                            RowClick::AudioSelectTriggerMode(pi, m) => {
                                vec![PanelAction::AudioModSetTriggerMode(target, self.modifier_card.pid_at(pi), m)]
                            }
                            RowClick::AudioSelectAction(pi, k) => {
                                vec![PanelAction::AudioModSetActionKind(target, self.modifier_card.pid_at(pi), k)]
                            }
                            RowClick::AudioSelectWrap(pi, w) => {
                                vec![PanelAction::AudioModSetWrap(target, self.modifier_card.pid_at(pi), w)]
                            }
                            RowClick::LabelCopy(_) => Vec::new(),
                            RowClick::EnumValueCell(pi) => {
                                self.modifier_card.enum_value_cell_action(pi, *node_id)
                            }
                        });
                    } else if let Some((pi, tab)) = self.modifier_card.mod_tab_hit(*node_id) {
                        self.modifier_card.focus_mod_tab(pi, tab);
                        actions.push(PanelAction::ModConfigTabChanged);
                    }
                }
                match &self.state {
                    SceneSetupState::NoGenerator { layer_id } if self.new_scene_id == Some(*node_id) => {
                        actions.push(PanelAction::SceneSetupNewScene(layer_id.clone()));
                    }
                    SceneSetupState::NoScene { layer_id } if self.open_graph_editor_id == Some(*node_id) => {
                        actions.push(PanelAction::SceneSetupOpenGraphEditor(layer_id.clone()));
                    }
                    _ => {}
                }
                (!actions.is_empty() || *node_id == self.close_id, actions)
            }
            // P4 (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D8): double-click on a
            // drag-armable value cell opens its type-in box. Reuses the
            // EXACT lookup PointerDown uses below (object/light/camera
            // value cells + every slider's value-text cell, world_card
            // included since C-P1a) — the same set arms both gestures by
            // construction, which is the drag/type-in registration parity
            // `dock_numeric_cells_register_full_contract` checks.
            UIEvent::DoubleClick { node_id, .. } => {
                if let SceneSetupState::Live(vm) = &self.state {
                    let intent = crate::value_cell::ValueCell::intent_for(
                        crate::value_cell::ValueCellZone::Cell,
                        crate::value_cell::ValueCellGesture::DoubleClick,
                    );
                    debug_assert_eq!(
                        intent,
                        Some(crate::value_cell::ValueCellIntent::EditValue),
                        "DoubleClick on a value cell must resolve to EditValue (D8's contract)"
                    );
                    let row_value = self
                        .world_row_value_for_value_text(*node_id)
                        // C-P1b: the converted Object family's own card
                        // slider value-text cells — same lookup shape as
                        // `world_row_value_for_value_text`, generalized.
                        .or_else(|| self.object_row_value_for_value_text(*node_id))
                        // C-P1c: the converted Light/Camera families' own
                        // card slider value-text cells — same lookup shape.
                        .or_else(|| self.light_row_value_for_value_text(*node_id))
                        .or_else(|| self.camera_row_value_for_value_text(*node_id))
                        // C-P1d: the converted Modifier family's own card
                        // slider value-text cells — same lookup shape, the
                        // last family to move off the old bespoke
                        // `object_value_cells` direct-vector lookup.
                        .or_else(|| self.modifier_row_value_for_value_text(*node_id));
                    if let Some(row_value) = row_value
                        && !row_value.driven
                        && !self.is_enum_value_cell(*node_id)
                        && intent == Some(crate::value_cell::ValueCellIntent::EditValue)
                    {
                        return (
                            true,
                            vec![PanelAction::SceneSetupBeginNumericTextInput {
                                layer_id: vm.layer_id.clone(),
                                scope_path: row_value.addr.scope_path.clone(),
                                node_doc_id: row_value.addr.node_doc_id,
                                param_id: row_value.addr.param_id.clone(),
                                value: row_value.value,
                                cell_node_id: *node_id,
                                degrees: is_degrees_param(&row_value.addr.param_id),
                            }],
                        );
                    }
                }
                (false, Vec::new())
            }
            UIEvent::PointerDown { node_id, pos, .. } => {
                if let SceneSetupState::Live(vm) = &self.state {
                    // C-P1a (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md D4): the
                    // converted Environment/Fog rows' slider tracks —
                    // absolute-position track-hit (a click anywhere on the
                    // track jumps straight to that value; drag continues
                    // absolute-position), dispatching the card drag protocol
                    // (`ParamSnapshot` + `ParamChanged`) instead of a bespoke
                    // per-tick command, so a whole scrub gesture is ONE undo
                    // unit (`ParamCommit` on release, in the `DragEnd`/
                    // `PointerUp` arm below). Checked before the delta-drag
                    // value cells below since a slider's track is a distinct
                    // node.
                    if let Some((pi, new_value)) = self
                        .world_card
                        .drag_sliders
                        .iter_mut()
                        .enumerate()
                        .find_map(|(pi, sl)| sl.try_start_drag(*node_id, pos.x).map(|v| (pi, v)))
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        let target = GraphParamTarget::Generator;
                        let pid = self.world_card.pid_at(pi);
                        return (
                            true,
                            vec![
                                PanelAction::ParamSnapshot(target, pid.clone()),
                                PanelAction::ParamChanged(target, pid, new_value),
                            ],
                        );
                    }
                    // C-P1b: the converted Object rows' slider tracks — same
                    // shape as `world_card` above, generalized.
                    if let Some((pi, new_value)) = self
                        .object_card
                        .drag_sliders
                        .iter_mut()
                        .enumerate()
                        .find_map(|(pi, sl)| sl.try_start_drag(*node_id, pos.x).map(|v| (pi, v)))
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        let target = GraphParamTarget::Generator;
                        let pid = self.object_card.pid_at(pi);
                        return (
                            true,
                            vec![
                                PanelAction::ParamSnapshot(target, pid.clone()),
                                PanelAction::ParamChanged(target, pid, new_value),
                            ],
                        );
                    }
                    // C-P1c: the converted Light rows' slider tracks — same
                    // shape as `world_card`/`object_card` above.
                    if let Some((pi, new_value)) = self
                        .light_card
                        .drag_sliders
                        .iter_mut()
                        .enumerate()
                        .find_map(|(pi, sl)| sl.try_start_drag(*node_id, pos.x).map(|v| (pi, v)))
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        let target = GraphParamTarget::Generator;
                        let pid = self.light_card.pid_at(pi);
                        return (
                            true,
                            vec![
                                PanelAction::ParamSnapshot(target, pid.clone()),
                                PanelAction::ParamChanged(target, pid, new_value),
                            ],
                        );
                    }
                    // C-P1c: the converted Camera rows' slider tracks — same
                    // shape as `world_card`/`object_card`/`light_card` above.
                    if let Some((pi, new_value)) = self
                        .camera_card
                        .drag_sliders
                        .iter_mut()
                        .enumerate()
                        .find_map(|(pi, sl)| sl.try_start_drag(*node_id, pos.x).map(|v| (pi, v)))
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        let target = GraphParamTarget::Generator;
                        let pid = self.camera_card.pid_at(pi);
                        return (
                            true,
                            vec![
                                PanelAction::ParamSnapshot(target, pid.clone()),
                                PanelAction::ParamChanged(target, pid, new_value),
                            ],
                        );
                    }
                    // C-P1d: the converted Modifier rows' slider tracks —
                    // same shape as `world_card`/`object_card`/`light_card`/
                    // `camera_card` above, the last family off the old
                    // bespoke `object_value_cells`+`ValueDrag` delta-drag
                    // path (deleted this phase).
                    if let Some((pi, new_value)) = self
                        .modifier_card
                        .drag_sliders
                        .iter_mut()
                        .enumerate()
                        .find_map(|(pi, sl)| sl.try_start_drag(*node_id, pos.x).map(|v| (pi, v)))
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        let target = GraphParamTarget::Generator;
                        let pid = self.modifier_card.pid_at(pi);
                        return (
                            true,
                            vec![
                                PanelAction::ParamSnapshot(target, pid.clone()),
                                PanelAction::ParamChanged(target, pid, new_value),
                            ],
                        );
                    }
                }
                (self.owns_node(*node_id) || self.point_in_panel(*pos), Vec::new())
            }
            UIEvent::DragBegin { .. } => (
                self.world_card.drag_sliders.iter().any(|s| s.is_dragging())
                    || self.object_card.drag_sliders.iter().any(|s| s.is_dragging())
                    || self.light_card.drag_sliders.iter().any(|s| s.is_dragging())
                    || self.camera_card.drag_sliders.iter().any(|s| s.is_dragging())
                    // C-P1d: the converted Modifier rows — same shape.
                    || self.modifier_card.drag_sliders.iter().any(|s| s.is_dragging()),
                Vec::new(),
            ),
            UIEvent::Drag { pos, .. } => {
                // C-P1a: continue an active slider drag first — one `find_map`
                // per family, in priority order. Live `ParamChanged` only, no
                // undo unit (the card cadence: one `ParamCommit` fires on
                // release, below). C-P1d: Modifier is the last family to move
                // onto this shape — no bespoke delta-drag (`self.drag`/
                // `ValueDrag`) remains anywhere in this panel.
                if self.drag_layer_id.is_some() {
                    if let Some((pi, new_value)) = self
                        .world_card
                        .drag_sliders
                        .iter()
                        .enumerate()
                        .find_map(|(pi, sl)| slider_drag_value(sl, pos.x).map(|v| (pi, v)))
                    {
                        let target = GraphParamTarget::Generator;
                        let pid = self.world_card.pid_at(pi);
                        return (true, vec![PanelAction::ParamChanged(target, pid, new_value)]);
                    }
                    // C-P1b: the converted Object rows — same shape.
                    if let Some((pi, new_value)) = self
                        .object_card
                        .drag_sliders
                        .iter()
                        .enumerate()
                        .find_map(|(pi, sl)| slider_drag_value(sl, pos.x).map(|v| (pi, v)))
                    {
                        let target = GraphParamTarget::Generator;
                        let pid = self.object_card.pid_at(pi);
                        return (true, vec![PanelAction::ParamChanged(target, pid, new_value)]);
                    }
                    // C-P1c: the converted Light rows — same shape.
                    if let Some((pi, new_value)) = self
                        .light_card
                        .drag_sliders
                        .iter()
                        .enumerate()
                        .find_map(|(pi, sl)| slider_drag_value(sl, pos.x).map(|v| (pi, v)))
                    {
                        let target = GraphParamTarget::Generator;
                        let pid = self.light_card.pid_at(pi);
                        return (true, vec![PanelAction::ParamChanged(target, pid, new_value)]);
                    }
                    // C-P1c: the converted Camera rows — same shape.
                    if let Some((pi, new_value)) = self
                        .camera_card
                        .drag_sliders
                        .iter()
                        .enumerate()
                        .find_map(|(pi, sl)| slider_drag_value(sl, pos.x).map(|v| (pi, v)))
                    {
                        let target = GraphParamTarget::Generator;
                        let pid = self.camera_card.pid_at(pi);
                        return (true, vec![PanelAction::ParamChanged(target, pid, new_value)]);
                    }
                    // C-P1d: the converted Modifier rows — same shape.
                    if let Some((pi, new_value)) = self
                        .modifier_card
                        .drag_sliders
                        .iter()
                        .enumerate()
                        .find_map(|(pi, sl)| slider_drag_value(sl, pos.x).map(|v| (pi, v)))
                    {
                        let target = GraphParamTarget::Generator;
                        let pid = self.modifier_card.pid_at(pi);
                        return (true, vec![PanelAction::ParamChanged(target, pid, new_value)]);
                    }
                }
                (false, Vec::new())
            }
            UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                // C-P1a (D4): release commits ONE undo unit for whichever row
                // was mid-drag, if any — the card protocol's Commit step. No
                // bespoke `self.drag.release()` left to call (C-P1d deleted
                // the last consumer, `ValueDrag`).
                let mut actions = Vec::new();
                for pi in 0..self.world_card.drag_sliders.len() {
                    if self.world_card.drag_sliders[pi].end_drag() {
                        let pid = self.world_card.pid_at(pi);
                        actions.push(PanelAction::ParamCommit(GraphParamTarget::Generator, pid));
                    }
                }
                // C-P1b: the converted Object rows — same shape.
                for pi in 0..self.object_card.drag_sliders.len() {
                    if self.object_card.drag_sliders[pi].end_drag() {
                        let pid = self.object_card.pid_at(pi);
                        actions.push(PanelAction::ParamCommit(GraphParamTarget::Generator, pid));
                    }
                }
                // C-P1c: the converted Light/Camera rows — same shape.
                for pi in 0..self.light_card.drag_sliders.len() {
                    if self.light_card.drag_sliders[pi].end_drag() {
                        let pid = self.light_card.pid_at(pi);
                        actions.push(PanelAction::ParamCommit(GraphParamTarget::Generator, pid));
                    }
                }
                for pi in 0..self.camera_card.drag_sliders.len() {
                    if self.camera_card.drag_sliders[pi].end_drag() {
                        let pid = self.camera_card.pid_at(pi);
                        actions.push(PanelAction::ParamCommit(GraphParamTarget::Generator, pid));
                    }
                }
                // C-P1d: the converted Modifier rows — same shape.
                for pi in 0..self.modifier_card.drag_sliders.len() {
                    if self.modifier_card.drag_sliders[pi].end_drag() {
                        let pid = self.modifier_card.pid_at(pi);
                        actions.push(PanelAction::ParamCommit(GraphParamTarget::Generator, pid));
                    }
                }
                self.drag_layer_id = None;
                (!actions.is_empty(), actions)
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

    fn owns_node(&self, node_id: NodeId) -> bool {
        node_id == self.bg_id
    }

    /// BUG-250: fan the enum-cell test out across all five family cards —
    /// the DoubleClick type-in arm skips these cells (their single click
    /// cycles/opens the dropdown instead).
    fn is_enum_value_cell(&self, node_id: NodeId) -> bool {
        [
            &self.world_card,
            &self.object_card,
            &self.light_card,
            &self.camera_card,
            &self.modifier_card,
        ]
        .into_iter()
        .any(|c| c.is_enum_value_cell(node_id))
    }

    /// C-P1a: `world_card`'s per-row value-text cell, for the DoubleClick
    /// type-in gesture — mirrors the metallic/roughness slider lookup just
    /// above it (same `SliderNodeIds.value_text` contract), reading the
    /// row's write address + current value back out of `id_map` (D2)
    /// instead of a `RowIds`/`row_index` table (deleted with the bespoke
    /// builder). `min`/`max`/`exposed` are unused by the DoubleClick action
    /// and left at harmless defaults.
    fn world_row_value_for_value_text(&self, node_id: NodeId) -> Option<RowValue> {
        let pi = self
            .world_card
            .slider_ids
            .iter()
            .position(|ids| ids.is_some_and(|ids| ids.value_text == node_id))?;
        let pid = self.world_card.pid_at(pi);
        let (addr, value) = self.world_card.id_map.get(&pid)?;
        Some(RowValue { addr: addr.clone(), value: *value, min: 0.0, max: 0.0, driven: false, exposed: false })
    }

    /// C-P1b: the Object-family twin of `world_row_value_for_value_text`.
    fn object_row_value_for_value_text(&self, node_id: NodeId) -> Option<RowValue> {
        let pi = self
            .object_card
            .slider_ids
            .iter()
            .position(|ids| ids.is_some_and(|ids| ids.value_text == node_id))?;
        let pid = self.object_card.pid_at(pi);
        let (addr, value) = self.object_card.id_map.get(&pid)?;
        Some(RowValue { addr: addr.clone(), value: *value, min: 0.0, max: 0.0, driven: false, exposed: false })
    }

    /// C-P1c: the Light-family twin of `world_row_value_for_value_text`.
    fn light_row_value_for_value_text(&self, node_id: NodeId) -> Option<RowValue> {
        let pi = self
            .light_card
            .slider_ids
            .iter()
            .position(|ids| ids.is_some_and(|ids| ids.value_text == node_id))?;
        let pid = self.light_card.pid_at(pi);
        let (addr, value) = self.light_card.id_map.get(&pid)?;
        Some(RowValue { addr: addr.clone(), value: *value, min: 0.0, max: 0.0, driven: false, exposed: false })
    }

    /// C-P1c: the Camera-family twin of `world_row_value_for_value_text`.
    fn camera_row_value_for_value_text(&self, node_id: NodeId) -> Option<RowValue> {
        let pi = self
            .camera_card
            .slider_ids
            .iter()
            .position(|ids| ids.is_some_and(|ids| ids.value_text == node_id))?;
        let pid = self.camera_card.pid_at(pi);
        let (addr, value) = self.camera_card.id_map.get(&pid)?;
        Some(RowValue { addr: addr.clone(), value: *value, min: 0.0, max: 0.0, driven: false, exposed: false })
    }

    /// C-P1d: the Modifier-family twin of `world_row_value_for_value_text` —
    /// the last family to move off the old direct `object_value_cells`
    /// vector lookup.
    fn modifier_row_value_for_value_text(&self, node_id: NodeId) -> Option<RowValue> {
        let pi = self
            .modifier_card
            .slider_ids
            .iter()
            .position(|ids| ids.is_some_and(|ids| ids.value_text == node_id))?;
        let pid = self.modifier_card.pid_at(pi);
        let (addr, value) = self.modifier_card.id_map.get(&pid)?;
        Some(RowValue { addr: addr.clone(), value: *value, min: 0.0, max: 0.0, driven: false, exposed: false })
    }

    /// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md D2: resolve a card-shaped
    /// action's synthesized `ParamId` back to this frame's write address +
    /// snapshot value. `dispatch_inspector`'s `ParamSnapshot`/`ParamChanged`/
    /// `ParamCommit` arms check this FIRST — a hit means the id addresses a
    /// converted scene row (routes through `SetGraphNodeParamCommand`); a
    /// miss falls through to the existing `with_preset_graph_mut` exposed-
    /// param path unchanged. C-P1c: checks `light_card`/`camera_card` too;
    /// C-P1d adds `modifier_card` — every card's map is disjoint by
    /// construction (`synth_world_param_id`'s `node_doc_id` is document-wide
    /// unique).
    pub fn resolve_scene_param(&self, id: &manifold_foundation::ParamId) -> Option<(RowAddr, f32)> {
        self.world_card
            .id_map
            .get(id)
            .or_else(|| self.object_card.id_map.get(id))
            .or_else(|| self.light_card.id_map.get(id))
            .or_else(|| self.camera_card.id_map.get(id))
            .or_else(|| self.modifier_card.id_map.get(id))
            .cloned()
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

/// D10's committed degrees-display row table, checked by `param_id` alone:
/// `transform_3d.rot_x/y/z`, `orbit_camera.orbit`/`tilt`, `free_camera`'s
/// `yaw`/`pitch`/`roll`, and `fov_y` (shared by all three camera atoms).
/// These names are otherwise unambiguous within the dock's curated
/// vocabulary, so a bare string match is the whole boundary — conversion
/// lives ONLY here + the triplet/camera-row formatters and the drag/type-in
/// commit paths that consult this fn; graph defs, commands, cards, and node
/// faces stay in radians, untouched.
fn is_degrees_param(param_id: &str) -> bool {
    matches!(param_id, "rot_x" | "rot_y" | "rot_z" | "orbit" | "tilt" | "yaw" | "pitch" | "roll" | "fov_y")
}

/// UX-P2 (D2): the value an active object slider drag resolves to at
/// `pos_x`, computed from its OWN `track_rect` — the exact math
/// [`crate::slider::SliderDragState::apply_drag`] uses, minus that method's
/// tree-mutating visual update. `handle_event` has no `&mut UITree` (the
/// panel's whole event surface is tree-free by design), so the slider's
/// fill/thumb/value-box don't update mid-drag locally; they update on the
/// SAME cadence the triplet cells' drag-scrub already does — the next
/// `build_nodes` pass after the round trip lands (D1: no per-frame
/// rebuild). Returns `None` when the slider isn't currently dragging.
fn slider_drag_value(slider: &crate::slider::SliderDragState, pos_x: f32) -> Option<f32> {
    if !slider.is_dragging() {
        return None;
    }
    let ids = slider.ids()?;
    let norm = crate::slider::BitmapSlider::x_to_normalized(ids.track_rect, pos_x);
    Some(crate::slider::BitmapSlider::normalized_to_value(norm, slider.min, slider.max))
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

fn btn_style() -> UIStyle {
    UIStyle { font_size: color::FONT_LABEL, ..crate::chrome::components::segment_style(false) }
}

fn label_style() -> UIStyle {
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
/// (the affordance-legibility rule: DESIGN_DOC_STANDARD §5). UX-P2 (D3c):
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

    fn env_row(value: f32) -> RowValue {
        RowValue { addr: RowAddr::root(3, "intensity"), value, min: 0.0, max: 4.0, driven: false, exposed: false }
    }

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
            audio_send_labels: Vec::new(),
            audio_send_ids: Vec::new(),
            objects: Vec::new(),
            lights: Vec::new(),
            camera: CameraRowVm::None,
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.add_environment_id.is_some());
        assert!(panel.add_fog_id.is_some());
        assert!(panel.add_object_id.is_some());
        assert!(panel.add_light_id.is_some());
    }

    #[test]
    fn driven_row_has_no_steppers() {
        let mut panel = ScenePanel::new();
        panel.open();
        let mut intensity = env_row(1.0);
        intensity.driven = true;
        panel.configure(SceneSetupState::Live(Box::new(SceneSetupVm {
            layer_id: LayerId::new("layer-1"),
            scene_name: "Scene".to_string(),
            multiple_scenes: false,
            object_count: 0,
            light_count: 0,
            shadow_caster_count: 0,
            scene_root_node_id: 0,
            environment: EnvironmentRowVm::Bare { intensity: mrow(intensity), fill: mrow(env_row(0.0)) },
            atmosphere: AtmosphereRowVm::None,
            audio_send_labels: Vec::new(),
            audio_send_ids: Vec::new(),
            objects: Vec::new(),
            lights: Vec::new(),
            camera: CameraRowVm::None,
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // C-P1a: a driven row builds no `build_param_row` slider at all
        // (the read-only dimmed render instead) — `world_card.slider_ids`
        // stays `None` at that row's fixed slot — and the read-only value
        // label's sync handle is kept instead, so the per-frame value sync
        // can update the driven row (it used to be discarded, freezing the
        // row between structural syncs).
        assert!(panel.world_card.slider_ids[WORLD_ENV_INTENSITY].is_none());
        assert!(
            panel.world_card.driven_value_ids[WORLD_ENV_INTENSITY].is_some(),
            "driven row must keep its value-label sync handle"
        );
        // A per-frame value sync resolving the row to 2.0 must rewrite the
        // read-only label's text in place — no rebuild.
        panel.sync_row_values(&mut tree, &|_addr| Some(2.0));
        assert!(
            tree.nodes().iter().any(|n| n.text.as_deref() == Some("2.00 (driven)")),
            "sync_row_values must push the fresh driven value onto the label"
        );
    }

    /// SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md §3 "Panel id map is total":
    /// every built row's synthesized id resolves through `resolve_scene_param`
    /// back to its own `RowAddr` — all four Environment/Fog rows wired this
    /// frame, none driven.
    #[test]
    fn world_id_map_is_total_over_every_built_row() {
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
            environment: EnvironmentRowVm::Bare {
                intensity: mrow(RowValue { addr: RowAddr::root(3, "intensity"), value: 1.0, min: 0.0, max: 4.0, driven: false, exposed: false }),
                fill: mrow(RowValue { addr: RowAddr::root(3, "fill"), value: 0.5, min: 0.0, max: 2.0, driven: false, exposed: false }),
            },
            atmosphere: AtmosphereRowVm::Wired {
                density: mrow(RowValue { addr: RowAddr::root(9, "density"), value: 0.2, min: 0.0, max: 1.0, driven: false, exposed: false }),
                height_falloff: mrow(RowValue { addr: RowAddr::root(9, "height_falloff"), value: 0.7, min: 0.0, max: 2.0, driven: false, exposed: false }),
            },
            audio_send_labels: Vec::new(),
            audio_send_ids: Vec::new(),
            objects: Vec::new(),
            lights: Vec::new(),
            camera: CameraRowVm::None,
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        assert_eq!(panel.world_card.id_map.len(), WORLD_ROW_COUNT, "all four rows are wired and non-driven");
        let expected = [
            (WORLD_ENV_INTENSITY, 3u32, "intensity"),
            (WORLD_ENV_FILL, 3u32, "fill"),
            (WORLD_FOG_DENSITY, 9u32, "density"),
            (WORLD_FOG_HEIGHT_FALLOFF, 9u32, "height_falloff"),
        ];
        for (slot, node_doc_id, param_key) in expected {
            let pid = panel.world_card.pid_at(slot);
            let (addr, _value) = panel
                .resolve_scene_param(&pid)
                .unwrap_or_else(|| panic!("slot {slot} id {pid:?} must resolve through the id map"));
            assert_eq!(addr.node_doc_id, node_doc_id, "slot {slot} write address must target its own node");
            assert_eq!(addr.param_id, param_key, "slot {slot} write address must target its own param");
        }
    }

    /// C-P1b (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md): id-map totality for
    /// the Object family — the twin of `world_id_map_is_total_over_every_
    /// built_row` above, extended over `object_card`. The azalea fixture's
    /// PBR object wires every one of the 14 fixed slots
    /// (`OBJ_ROW_POS_X`..`OBJ_ROW_ROUGHNESS`), so every slot's synthesized
    /// id must resolve back through `resolve_scene_param` to its own
    /// (node_doc_id, param_key) write address.
    #[test]
    fn object_id_map_is_total_over_every_built_row() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        assert_eq!(panel.object_card.id_map.len(), OBJ_ROW_COUNT, "every Object row is wired and non-driven for a PBR object");
        // The shared `triplet()`/`mtriplet()` fixture helpers stamp a
        // generic "x"/"y"/"z" `RowAddr.param_id` for every triplet (pos/rot/
        // scale/color alike, node ids 50/50/50/51) — the SYNTHESIZED id
        // (what `resolve_scene_param` is keyed by) uses the real
        // `pos_x`/`rot_x`/… name from `build_object_card_row`'s own
        // `param_key` argument, but the resolved WRITE ADDRESS this table
        // checks is the fixture's own (generic-named) `RowAddr` — same
        // distinction `rotation_row_displays_degrees_not_radians` calls out.
        let expected = [
            (OBJ_ROW_POS_X, 50u32, "x"),
            (OBJ_ROW_POS_Y, 50u32, "y"),
            (OBJ_ROW_POS_Z, 50u32, "z"),
            (OBJ_ROW_ROT_X, 50u32, "x"),
            (OBJ_ROW_ROT_Y, 50u32, "y"),
            (OBJ_ROW_ROT_Z, 50u32, "z"),
            (OBJ_ROW_SCALE_X, 50u32, "x"),
            (OBJ_ROW_SCALE_Y, 50u32, "y"),
            (OBJ_ROW_SCALE_Z, 50u32, "z"),
            (OBJ_ROW_COLOR_R, 51u32, "x"),
            (OBJ_ROW_COLOR_G, 51u32, "y"),
            (OBJ_ROW_COLOR_B, 51u32, "z"),
            (OBJ_ROW_METALLIC, 51u32, "metallic"),
            (OBJ_ROW_ROUGHNESS, 51u32, "roughness"),
        ];
        for (slot, node_doc_id, param_key) in expected {
            let pid = panel.object_card.pid_at(slot);
            let (addr, _value) = panel
                .resolve_scene_param(&pid)
                .unwrap_or_else(|| panic!("slot {slot} id {pid:?} must resolve through the id map"));
            assert_eq!(addr.node_doc_id, node_doc_id, "slot {slot} write address must target its own node");
            assert_eq!(addr.param_id, param_key, "slot {slot} write address must target its own param");
        }
    }

    /// C-P1c: id-map totality for the Light family — the twin of
    /// `object_id_map_is_total_over_every_built_row`, extended over
    /// `light_card`. The azalea fixture's Known light wires every one of the
    /// 14 fixed slots (`LIGHT_ROW_MODE`..`LIGHT_ROW_LIGHT_SIZE`).
    #[test]
    fn light_id_map_is_total_over_every_built_row() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        assert_eq!(panel.light_card.id_map.len(), LIGHT_ROW_COUNT, "every Light row is wired and non-driven for a Known light");
        // The shared `triplet()`/`mtriplet()` fixture helper stamps a
        // generic "x"/"y"/"z" `RowAddr.param_id` for color/pos/aim alike
        // (same distinction `object_id_map_is_total_over_every_built_row`
        // calls out) — the resolved WRITE ADDRESS this table checks is the
        // fixture's own generic-named `RowAddr`, not the synthesized id.
        let expected = [
            (LIGHT_ROW_MODE, 60u32, "mode"),
            (LIGHT_ROW_COLOR_R, 60u32, "x"),
            (LIGHT_ROW_COLOR_G, 60u32, "y"),
            (LIGHT_ROW_COLOR_B, 60u32, "z"),
            (LIGHT_ROW_INTENSITY, 60u32, "intensity"),
            (LIGHT_ROW_POS_X, 60u32, "x"),
            (LIGHT_ROW_POS_Y, 60u32, "y"),
            (LIGHT_ROW_POS_Z, 60u32, "z"),
            (LIGHT_ROW_AIM_X, 60u32, "x"),
            (LIGHT_ROW_AIM_Y, 60u32, "y"),
            (LIGHT_ROW_AIM_Z, 60u32, "z"),
            (LIGHT_ROW_CAST_SHADOWS, 60u32, "cast_shadows"),
            (LIGHT_ROW_SHADOW_SOFTNESS, 60u32, "shadow_softness"),
            (LIGHT_ROW_LIGHT_SIZE, 60u32, "light_size"),
        ];
        for (slot, node_doc_id, param_key) in expected {
            let pid = panel.light_card.pid_at(slot);
            let (addr, _value) = panel
                .resolve_scene_param(&pid)
                .unwrap_or_else(|| panic!("slot {slot} id {pid:?} must resolve through the id map"));
            assert_eq!(addr.node_doc_id, node_doc_id, "slot {slot} write address must target its own node");
            assert_eq!(addr.param_id, param_key, "slot {slot} write address must target its own param");
        }
    }

    /// C-P1c: id-map totality for the Camera family — the twin above,
    /// extended over `camera_card`. The azalea fixture's `OrbitCameraRowVm`
    /// wires 8 of `CAM_ROW_COUNT`'s 17 slots (the Free/LookAt-only fields
    /// stay unpopulated, same "only every WIRED row lands in the map"
    /// contract established for World/Object).
    #[test]
    fn camera_id_map_is_total_over_every_built_row() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let expected = [
            (CAM_ROW_ORBIT, 70u32, "orbit"),
            (CAM_ROW_TILT, 70u32, "tilt"),
            (CAM_ROW_DISTANCE, 70u32, "distance"),
            (CAM_ROW_FOV, 70u32, "fov_y"),
            (CAM_ROW_LENS_FOCUS, 71u32, "focus_distance"),
            (CAM_ROW_LENS_FSTOP, 71u32, "f_stop"),
            (CAM_ROW_LENS_SHUTTER, 71u32, "shutter_angle"),
            (CAM_ROW_LENS_EXPOSURE, 71u32, "exposure_ev"),
        ];
        assert_eq!(panel.camera_card.id_map.len(), expected.len(), "only the Orbit shape's own fields wire");
        for (slot, node_doc_id, param_key) in expected {
            let pid = panel.camera_card.pid_at(slot);
            let (addr, _value) = panel
                .resolve_scene_param(&pid)
                .unwrap_or_else(|| panic!("slot {slot} id {pid:?} must resolve through the id map"));
            assert_eq!(addr.node_doc_id, node_doc_id, "slot {slot} write address must target its own node");
            assert_eq!(addr.param_id, param_key, "slot {slot} write address must target its own param");
        }
    }

    /// C-P1d: id-map totality for the Modifier family — the same shape as
    /// `light_id_map_is_total_over_every_built_row`, but `modifier_card` has
    /// no fixed `*_ROW_COUNT` (a stack is a variable-length list, per
    /// `modifier_card`'s own doc comment) — the azalea fixture's single Bend
    /// modifier wires exactly its 2 rows (Axis, Angle), running-slot 0 and 1
    /// in wire order.
    #[test]
    fn modifier_id_map_is_total_over_every_built_row() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        // azalea_shaped_vm's default selection already targets the Azalea object.
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let expected = [(0usize, 70u32, "axis"), (1usize, 70u32, "angle")];
        assert_eq!(panel.modifier_card.id_map.len(), expected.len(), "Bend's own two rows, no more no less");
        for (slot, node_doc_id, param_key) in expected {
            let pid = panel.modifier_card.pid_at(slot);
            let (addr, _value) = panel
                .resolve_scene_param(&pid)
                .unwrap_or_else(|| panic!("slot {slot} id {pid:?} must resolve through the id map"));
            assert_eq!(addr.node_doc_id, node_doc_id, "slot {slot} write address must target its own node");
            assert_eq!(addr.param_id, param_key, "slot {slot} write address must target its own param");
        }
    }

    /// C-P1d (D4): the Modifier-family twin of
    /// `light_intensity_card_row_sweeps_full_range_with_one_commit` —
    /// scrubbing the Bend modifier's Angle row via the real card slider
    /// track is ONE undo unit (PointerDown snapshots + jumps, Drag
    /// continues live, PointerUp commits exactly once).
    #[test]
    fn modifier_angle_card_row_sweeps_full_range_with_one_commit() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Slot 1 = Angle (slot 0 is the Axis enum row) — see
        // `modifier_id_map_is_total_over_every_built_row`.
        let ids = panel.modifier_card.slider_ids[1].expect("Angle renders a card slider");
        let track = ids.track_rect;
        let target = GraphParamTarget::Generator;
        let pid = panel.modifier_card.pid_at(1);

        let (consumed, actions) = panel.handle_event(&UIEvent::PointerDown {
            node_id: ids.track,
            pos: Vec2::new(track.x, 0.0),
            modifiers: crate::input::Modifiers::default(),
        });
        assert!(consumed, "the Angle row's slider track must be drag-armable");
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::ParamSnapshot(t, p), PanelAction::ParamChanged(_, _, low)]
                if *t == target && *p == pid && *low < -6.0
        ), "PointerDown at the track's left edge must snapshot + land near min, got {actions:?}");

        let (drag_consumed, drag_actions) = panel.handle_event(&UIEvent::Drag {
            node_id: Some(ids.track),
            pos: Vec2::new(track.x + track.width, 0.0),
            delta: Vec2::ZERO,
        });
        assert!(drag_consumed);
        assert!(matches!(
            drag_actions.as_slice(),
            [PanelAction::ParamChanged(_, _, high)] if *high > 6.0
        ), "dragging to the track's right edge must land near max, got {drag_actions:?}");

        let (up_consumed, up_actions) = panel
            .handle_event(&UIEvent::PointerUp { node_id: Some(ids.track), pos: Vec2::new(track.x + track.width, 0.0) });
        assert!(up_consumed);
        assert!(
            matches!(up_actions.as_slice(), [PanelAction::ParamCommit(t, p)] if *t == target && *p == pid),
            "release must commit ONE ParamCommit, got {up_actions:?}"
        );
        assert!(!panel.modifier_card.drag_sliders[1].is_dragging(), "PointerUp ends the drag");
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
            audio_send_labels: Vec::new(),
            audio_send_ids: Vec::new(),
            objects: vec![
                ObjectRowVm::Known(Box::new(ObjectKnownRow {
                    index: 0,
                    object_node_id: 40,
                    group_node_id: Some(42),
                    name: "Azalea".to_string(),
                    visible: RowValue { addr: RowAddr { scope_path: vec![42], node_doc_id: 40, param_id: "visible".to_string() }, value: 1.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                    transform: Some(Box::new(TransformRowVm {
                        pos: mtriplet(50, 1.0, 2.0, 3.0, -100.0, 100.0),
                        rot: mtriplet(50, 0.0, 0.0, 0.0, -6.28, 6.28),
                        scale: mtriplet(50, 1.0, 1.0, 1.0, 0.01, 10.0),
                    })),
                    material: ObjectMaterialVm::Pbr {
                        color: mtriplet(51, 0.8, 0.8, 0.82, 0.0, 1.0),
                        metallic: mrow(RowValue { addr: RowAddr::root(51, "metallic"), value: 0.0, min: 0.0, max: 1.0, driven: false, exposed: false }),
                        roughness: mrow(RowValue { addr: RowAddr::root(51, "roughness"), value: 0.5, min: 0.01, max: 1.0, driven: false, exposed: false }),
                    },
                    modifiers: vec![ModifierKnownRow {
                        index: 0,
                        node_doc_id: 70,
                        display_name: "Bend".to_string(),
                        params: vec![
                            ModifierParamRowVm::Axis {
                                label: "Axis",
                                row: menum(
                                    RowValue { addr: RowAddr { scope_path: vec![42], node_doc_id: 70, param_id: "axis".to_string() }, value: 1.0, min: 0.0, max: 2.0, driven: false, exposed: false },
                                    vec!["X", "Y", "Z"],
                                ),
                            },
                            ModifierParamRowVm::Numeric {
                                label: "Angle",
                                row: mrow(RowValue { addr: RowAddr { scope_path: vec![42], node_doc_id: 70, param_id: "angle".to_string() }, value: 0.5, min: -6.28, max: 6.28, driven: false, exposed: false }),
                            },
                        ],
                    }],
                    modifiers_addable: true,
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
                })),
                LightRowVm::Custom { index: 1 },
            ],
            camera: CameraRowVm::Orbit(Box::new(OrbitCameraRowVm {
                orbit: mrow(RowValue { addr: RowAddr::root(70, "orbit"), value: 0.7, min: -6.28, max: 6.28, driven: false, exposed: false }),
                tilt: mrow(RowValue { addr: RowAddr::root(70, "tilt"), value: 0.3, min: -6.28, max: 6.28, driven: false, exposed: false }),
                distance: mrow(RowValue { addr: RowAddr::root(70, "distance"), value: 4.0, min: 0.01, max: 100.0, driven: false, exposed: false }),
                fov_y: mrow(RowValue { addr: RowAddr::root(70, "fov_y"), value: 0.9, min: 0.05, max: 2.5, driven: false, exposed: false }),
                lens: Some(LensRowVm {
                    focus_distance: mrow(RowValue { addr: RowAddr::root(71, "focus_distance"), value: 0.0, min: 0.0, max: 1000.0, driven: false, exposed: false }),
                    f_stop: mrow(RowValue { addr: RowAddr::root(71, "f_stop"), value: 1000.0, min: 0.5, max: 1000.0, driven: false, exposed: false }),
                    shutter_angle: mrow(RowValue { addr: RowAddr::root(71, "shutter_angle"), value: 0.0, min: 0.0, max: 360.0, driven: false, exposed: false }),
                    exposure_ev: mrow(RowValue { addr: RowAddr::root(71, "exposure_ev"), value: 0.0, min: -8.0, max: 8.0, driven: false, exposed: false }),
                }),
            })),
        }
    }

    #[test]
    fn objects_outliner_lists_known_and_custom_rows_properties_shows_the_selected_one() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Outliner rows: Camera + World + 1 Known light + 1 Known object are
        // selectable (`outliner_row_ids`); the Custom object/light are
        // listed too but as plain labels (D3: never hidden, but no
        // addressable node id to select by, D12).
        assert_eq!(panel.outliner_row_ids.len(), 4, "Camera + World + 1 known light + 1 known object");
        // Default selection (D7): the first Known object — Azalea — so its
        // properties header + body render without any click.
        assert_eq!(panel.object_name_ids.len(), 1, "the properties header shows the selected object's name");
        assert_eq!(panel.object_name_ids[0].0, 42, "resolves to the object's group node id (the rename address)");
        assert_eq!(panel.object_name_ids[0].2, "Azalea");
        // The full body always renders (no fold state left — the outliner
        // IS the fold). C-P1b moved Position/Rotation/Scale (9)/Color (3)/
        // Metallic/Roughness (2) onto the card-row protocol (`object_card`,
        // asserted below); C-P1d moved the Bend modifier's own two rows
        // (Axis + Angle) onto `modifier_card` too — every modifier param
        // row rides the shared card core now, enum/labeled rows included.
        // No bespoke `object_value_cells` vector remains anywhere in the
        // panel.
        assert_eq!(panel.modifier_card.id_map.len(), 2, "Bend's Axis + Angle rows both wired onto the card protocol");
        // 9 transform + 3 color + Metallic + Roughness = 14 wired card rows
        // — every `OBJ_ROW_*` slot the azalea fixture's PBR material
        // populates.
        assert_eq!(panel.object_card.id_map.len(), OBJ_ROW_COUNT, "every Object card row is wired for a PBR object");
        assert!(panel.add_object_id.is_some());
        assert!(panel.add_light_id.is_some());
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
                params: vec![],
            },
            ModifierKnownRow {
                index: 1,
                node_doc_id: 71,
                display_name: "Twist".to_string(),
                params: vec![],
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
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupAddModifierClicked(l, 42, n)
                if *l == LayerId::new("layer-1") && *n == button_id
        ));
    }

    /// C-P1b (SCENE_PANEL_CARD_CONVERGENCE_DESIGN.md D4): the Roughness card
    /// row's slider track is drag-armable (a click anywhere on the track
    /// jumps straight to that value via `ParamSnapshot`+`ParamChanged`, Drag
    /// continues live `ParamChanged` only, release commits ONE
    /// `ParamCommit` — the "sweep full-range in one drag, one undo unit"
    /// performer gesture D4 names), and its separate value-text box still
    /// opens the type-in box on double-click (D8 parity,
    /// `dock_numeric_cells_register_full_contract`'s sibling for the
    /// two-node slider shape that test's single-node loop can't cover).
    #[test]
    fn roughness_card_row_sweeps_full_range_and_value_box_opens_typein() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let ids = panel.object_card.slider_ids[OBJ_ROW_ROUGHNESS].expect("Roughness renders a slider");
        let track = ids.track_rect;
        let target = GraphParamTarget::Generator;
        let pid = panel.object_card.pid_at(OBJ_ROW_ROUGHNESS);

        // PointerDown at the track's left edge — jumps to (near) min.
        let (consumed, actions) = panel.handle_event(&UIEvent::PointerDown {
            node_id: ids.track,
            pos: Vec2::new(track.x, 0.0),
            modifiers: crate::input::Modifiers::default(),
        });
        assert!(consumed, "the slider track must be drag-armable");
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::ParamSnapshot(t, p), PanelAction::ParamChanged(_, _, low)]
                if *t == target && *p == pid && *low < 0.05
        ), "PointerDown at the track's left edge must snapshot + land near min, got {actions:?}");

        // Drag to the track's right edge — one gesture sweeps to (near) max,
        // continuing off the SAME `try_start_drag` arm via `slider_drag_value`.
        let (drag_consumed, drag_actions) = panel.handle_event(&UIEvent::Drag {
            node_id: Some(ids.track),
            pos: Vec2::new(track.x + track.width, 0.0),
            delta: Vec2::ZERO,
        });
        assert!(drag_consumed);
        assert!(matches!(
            drag_actions.as_slice(),
            [PanelAction::ParamChanged(_, _, high)] if *high > 0.95
        ), "dragging to the track's right edge must land near max, got {drag_actions:?}");

        let (up_consumed, up_actions) = panel
            .handle_event(&UIEvent::PointerUp { node_id: Some(ids.track), pos: Vec2::new(track.x + track.width, 0.0) });
        assert!(up_consumed);
        assert!(
            matches!(up_actions.as_slice(), [PanelAction::ParamCommit(t, p)] if *t == target && *p == pid),
            "release must commit ONE ParamCommit, got {up_actions:?}"
        );
        assert!(!panel.object_card.drag_sliders[OBJ_ROW_ROUGHNESS].is_dragging(), "PointerUp ends the drag");

        // The value box (a different node than the track) opens type-in.
        let (typein_consumed, typein_actions) = panel.handle_event(&UIEvent::DoubleClick {
            node_id: ids.value_text,
            pos: Vec2::ZERO,
            modifiers: crate::input::Modifiers::default(),
        });
        assert!(typein_consumed);
        assert!(matches!(
            typein_actions.as_slice(),
            [PanelAction::SceneSetupBeginNumericTextInput { param_id, .. }] if param_id == "roughness"
        ));
    }

    /// C-P1d's own acceptance-flow gate, at the render level (the
    /// `--script` headless harness can't independently confirm this half:
    /// `PanelAction::DriverToggle`'s dispatch is proven correct by log
    /// inspection — `cargo xtask ui-snap gltfscene --script
    /// scripts/ui-flows/scene-card-convergence-c-p1d-roughness-scrub-drawer-undo.json`
    /// shows the exact `DriverToggle(Generator, "scene.10.roughness")` fire
    /// — but the SAME class of harness gap BUG-234/BUG-239 already
    /// documented means the resulting drawer's own visible text isn't
    /// reliably observable through that same script run, so this proves
    /// the OTHER half directly: feed a Roughness row with
    /// `RowModulation.driver_active = true` — the exact shape
    /// `dispatch_inspector`'s `DriverToggle` arm produces on the next
    /// resync — and confirm `build_object_card_row`'s driven branch
    /// actually renders `build_driver_config`'s beat-grid drawer (the
    /// "Straight"/"Dotted"/"Triplet" feel row is deep enough into the
    /// drawer body to prove it built past the tab strip, not just reserved
    /// the slot).
    #[test]
    fn roughness_driver_active_renders_the_drawer_inline() {
        let mut vm = azalea_shaped_vm();
        if let ObjectRowVm::Known(row) = &mut vm.objects[0]
            && let ObjectMaterialVm::Pbr { roughness, .. } = &mut row.material
        {
            roughness.modulation.driver_active = true;
            roughness.modulation.driver_beat_div_idx = 2;
        }
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let has_text = |needle: &str| {
            (0..tree.count())
                .any(|i| tree.get_node(tree.id_at(i)).is_some_and(|n| n.text.as_deref() == Some(needle)))
        };
        assert!(has_text("Straight"), "an armed driver must render its config drawer INLINE in the panel");
        assert!(has_text("Dotted"));
        assert!(has_text("Triplet"));
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
        });
        assert!(consumed);
        assert!(
            matches!(actions.as_slice(), [PanelAction::OpenSceneSetup]),
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
        });
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupRemoveModifier(l, 42, 70) if *l == LayerId::new("layer-1")
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
        });
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupMoveModifier(l, 42, 71, 0) if *l == LayerId::new("layer-1")
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
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupAddObject(l, 99, 2) if *l == LayerId::new("layer-1")
        ));

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: add_light_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupAddLight(l, 99, 1) if *l == LayerId::new("layer-1")
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
        });
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupRemoveObject(l, 99, 0) if *l == LayerId::new("layer-1")
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
        });
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupDuplicateObject(l, 99, 0) if *l == LayerId::new("layer-1")
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
        // now key off `build_param_row`'s own `row_key_base` (`slot << 8`),
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
        // (`slot << 8`), same disjoint key space C-P1b established for
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
        // an explicit-key scheme anymore (`build_param_row`'s `slot << 8`
        // covers all its rows), so there is nothing left to audit here.

        // Modifier: per-slot offsets (up to 4 param slots) must fit inside
        // MODIFIER_ROW_STRIDE, same per-index-range contract as OBJECT/LIGHT.
        // C-P1d: the old `MODIFIER_OFF_PARAM_BASE` 3-wide `[-] value [+]`
        // stepper offsets are gone (deleted with the pre-convergence bespoke
        // numeric/enum stepper builders) — a Numeric/Axis row's own value
        // cell, track, and steppers now key through `build_param_row`'s internal
        // `(slot << 8)` scheme, not `modifier_row_key`; only the reorder/
        // remove chrome and the mod-button offset still use it.
        assert_no_dupes_and_fits_stride(
            "MODIFIER (per-row)",
            &[MODIFIER_OFF_UP, MODIFIER_OFF_DOWN, MODIFIER_OFF_REMOVE],
            Some(MODIFIER_ROW_STRIDE),
        );
    }

    /// The scene cards' sliders carry the SAME `Track + RightClick →
    /// SliderReset` contract every other slider in the app has — regression
    /// for the missing hookup where `build_param_row` constructed the resets
    /// but the panel never implemented `register_intents`, so right-click
    /// reset was dead on every scene row while working everywhere else.
    #[test]
    fn scene_row_slider_track_right_click_resolves_to_slider_reset() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let mut reg = crate::intent::IntentRegistry::new();
        panel.register_intents(&mut reg);

        let mut checked = 0;
        for ids in panel.object_card.slider_ids.iter().flatten() {
            match reg.resolve(&tree, Some(ids.track), crate::intent::Gesture::RightClick) {
                Some(PanelAction::SliderReset { .. }) => checked += 1,
                other => panic!("expected SliderReset on object-card track, got {other:?}"),
            }
        }
        assert!(checked > 0, "at least one object-card slider must be built");
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
        });
        assert!(consumed);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupRemoveLight(l, 99, 0) if *l == LayerId::new("layer-1")
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
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupImportModelClicked(l, 99) if *l == LayerId::new("layer-1")
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
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        assert!(matches!(
            &actions[0],
            PanelAction::SceneSetupRenameObjectClicked(l, 42, n)
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
        // Default selection = Azalea: object body cells present (C-P1b: the
        // card rows, not `object_value_cells` — Position/Rotation/Scale/
        // Color/Metallic/Roughness all moved onto `object_card`), no
        // environment/fog "add" affordances (azalea fixture's environment
        // is None — but World isn't selected, so neither button builds).
        assert!(!panel.object_card.id_map.is_empty(), "Azalea's body renders by default");
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
        });
        assert!(consumed);
        assert_eq!(panel.selection.get(&LayerId::new("layer-1")), Some(&SceneSelection::World));

        let mut tree2 = UITree::new();
        panel.build_docked(&mut tree2, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.object_card.id_map.is_empty(), "World selected — no object body renders");
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

    #[test]
    fn lights_outliner_lists_known_and_custom_rows_properties_shows_the_selected_one() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // C-P1c: the Known light's body now builds through `light_card` —
        // every LIGHT_ROW_* slot is wired (mode + color triplet + intensity
        // + pos triplet + aim triplet + cast_shadows + shadow_softness +
        // light_size = LIGHT_ROW_COUNT rows).
        assert_eq!(panel.light_card.id_map.len(), LIGHT_ROW_COUNT, "every Light card row is wired for a Known light");
        assert!(panel.add_light_id.is_some());
    }

    /// C-P1c: Mode/Cast Shadows/Shadow Softness build through the shared
    /// card row core with `ParamInfo.value_labels` set (D1's "check for a
    /// card enum row first and use it if one exists" — it does, no bespoke
    /// enum stepper needed) — `whole_numbers` is derived from `labels.is_some()`
    /// and each row's label count matches the VM's own `labels` field.
    #[test]
    fn light_enum_rows_carry_value_labels_on_the_card_row() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        for (slot, expected_labels) in [
            (LIGHT_ROW_MODE, 2),
            (LIGHT_ROW_CAST_SHADOWS, 2),
            (LIGHT_ROW_SHADOW_SOFTNESS, 4),
        ] {
            let info = &panel.light_card.param_info[slot];
            assert!(info.whole_numbers, "slot {slot} must be whole_numbers (labeled) — enum row");
            let labels = info.value_labels.as_ref().unwrap_or_else(|| panic!("slot {slot} must carry value_labels"));
            assert_eq!(labels.len(), expected_labels, "slot {slot} label count must match the VM's own labels");
        }
        // Color/Position/Aim/Intensity/Light Size are plain numeric — no labels.
        for slot in [LIGHT_ROW_COLOR_R, LIGHT_ROW_INTENSITY, LIGHT_ROW_POS_X, LIGHT_ROW_LIGHT_SIZE] {
            assert!(panel.light_card.param_info[slot].value_labels.is_none(), "slot {slot} must be plain numeric");
        }
    }

    #[test]
    fn light_size_row_always_renders_even_when_softness_isnt_contact() {
        // D4: light_size is a parameter DEPENDENCY, not conditional UI — the
        // card row must exist regardless of the current shadow_softness value.
        let mut vm = azalea_shaped_vm();
        if let LightRowVm::Known(row) = &mut vm.lights[0] {
            row.shadow_softness.row.value.value = 0.0; // Hard, not Contact
        }
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(
            panel.light_card.param_info[LIGHT_ROW_LIGHT_SIZE].param_id.contains("light_size"),
            "light_size card row exists regardless of shadow_softness mode"
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

    /// C-P1c: the Light Intensity card row's slider track is drag-armable
    /// through the same Begin/Changed/Commit protocol
    /// `roughness_card_row_sweeps_full_range_and_value_box_opens_typein`
    /// proves for Object — one full sweep is ONE undo unit (`ParamCommit` on
    /// release, not a `SceneSetupParamChanged` per motion tick). This is the
    /// exact gesture BUG-237 ("Camera/World/Light params do nothing") was
    /// filed against for the Light family.
    #[test]
    fn light_intensity_card_row_sweeps_full_range_with_one_commit() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let ids = panel.light_card.slider_ids[LIGHT_ROW_INTENSITY].expect("Intensity renders a slider");
        let track = ids.track_rect;
        let target = GraphParamTarget::Generator;
        let pid = panel.light_card.pid_at(LIGHT_ROW_INTENSITY);
        let (addr, _) = panel.resolve_scene_param(&pid).expect("intensity id resolves through the id map");
        assert_eq!(addr, RowAddr::root(60, "intensity"), "lights live at root scope, never inside a group");

        // PointerDown at the track's right edge — jumps toward max.
        let (consumed, actions) = panel.handle_event(&UIEvent::PointerDown {
            node_id: ids.track,
            pos: Vec2::new(track.x + track.width, 0.0),
            modifiers: crate::input::Modifiers::default(),
        });
        assert!(consumed, "the slider track must be drag-armable");
        assert!(matches!(
            actions.as_slice(),
            [PanelAction::ParamSnapshot(t, p), PanelAction::ParamChanged(_, _, v)]
                if *t == target && *p == pid && *v > 2.5
        ), "PointerDown near the track's right edge must snapshot + increase intensity above its 2.5 start, got {actions:?}");

        let (up_consumed, up_actions) = panel
            .handle_event(&UIEvent::PointerUp { node_id: Some(ids.track), pos: Vec2::new(track.x + track.width, 0.0) });
        assert!(up_consumed);
        assert!(
            matches!(up_actions.as_slice(), [PanelAction::ParamCommit(t, p)] if *t == target && *p == pid),
            "release must commit exactly ONE ParamCommit — one undo unit per gesture, got {up_actions:?}"
        );
        assert!(!panel.light_card.drag_sliders[LIGHT_ROW_INTENSITY].is_dragging(), "PointerUp ends the drag");
    }

    #[test]
    fn camera_section_renders_orbit_rows_and_lens_sub_section() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // C-P1c: Orbit/Tilt/Distance/FOV (4) + Lens's 4 fields = 8 wired
        // Camera card rows (the rest of CAM_ROW_COUNT's 17 slots are the
        // Free/LookAt-only fields the Orbit shape never populates).
        let expected: &[(usize, u32, &str)] = &[
            (CAM_ROW_ORBIT, 70, "orbit"),
            (CAM_ROW_TILT, 70, "tilt"),
            (CAM_ROW_DISTANCE, 70, "distance"),
            (CAM_ROW_FOV, 70, "fov_y"),
            (CAM_ROW_LENS_FOCUS, 71, "focus_distance"),
            (CAM_ROW_LENS_FSTOP, 71, "f_stop"),
            (CAM_ROW_LENS_SHUTTER, 71, "shutter_angle"),
            (CAM_ROW_LENS_EXPOSURE, 71, "exposure_ev"),
        ];
        assert_eq!(panel.camera_card.id_map.len(), expected.len(), "4 orbit rows + 4 lens rows");
        for (slot, node_doc_id, param_key) in expected {
            let pid = panel.camera_card.pid_at(*slot);
            let (addr, _) = panel
                .resolve_scene_param(&pid)
                .unwrap_or_else(|| panic!("slot {slot} id {pid:?} must resolve through the id map"));
            assert_eq!(addr.node_doc_id, *node_doc_id, "slot {slot} write address must target its own node");
            assert_eq!(addr.param_id, *param_key, "slot {slot} write address must target its own param");
        }
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

    #[test]
    fn free_and_look_at_camera_shapes_render_their_own_rows() {
        let free_cam = CameraRowVm::Free(Box::new(FreeCameraRowVm {
            pos: mtriplet(70, 1.0, 2.0, 3.0, -1000.0, 1000.0),
            yaw: mrow(RowValue { addr: RowAddr::root(70, "yaw"), value: 0.0, min: -6.28, max: 6.28, driven: false, exposed: false }),
            pitch: mrow(RowValue { addr: RowAddr::root(70, "pitch"), value: 0.0, min: -1.5, max: 1.5, driven: false, exposed: false }),
            roll: mrow(RowValue { addr: RowAddr::root(70, "roll"), value: 0.0, min: -6.28, max: 6.28, driven: false, exposed: false }),
            fov_y: mrow(RowValue { addr: RowAddr::root(70, "fov_y"), value: 0.9, min: 0.05, max: 2.5, driven: false, exposed: false }),
            lens: None,
        }));
        let mut vm = azalea_shaped_vm();
        vm.camera = free_cam;
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // C-P1c: Position triplet (3) + yaw + pitch + roll + fov = 7 wired
        // Camera card rows, no lens.
        assert_eq!(panel.camera_card.id_map.len(), 7);

        let look_at_cam = CameraRowVm::LookAt(Box::new(LookAtCameraRowVm {
            pos: mtriplet(70, 1.0, 2.0, 3.0, -1000.0, 1000.0),
            target: mtriplet(70, 0.0, 0.0, 0.0, -1000.0, 1000.0),
            fov_y: mrow(RowValue { addr: RowAddr::root(70, "fov_y"), value: 0.9, min: 0.05, max: 2.5, driven: false, exposed: false }),
            lens: None,
        }));
        let mut vm2 = azalea_shaped_vm();
        vm2.camera = look_at_cam;
        let mut panel2 = ScenePanel::new();
        panel2.open();
        panel2.configure(SceneSetupState::Live(Box::new(vm2)));
        panel2.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree2 = UITree::new();
        panel2.build_docked(&mut tree2, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Position triplet (3) + Target triplet (3) + fov = 7 wired rows.
        assert_eq!(panel2.camera_card.id_map.len(), 7);
    }

    /// SCENE_OBJECT_AND_PANEL_V2_DESIGN.md §4 invariant: every drag-armable
    /// value cell built by the dock is ALSO in the type-in registration set,
    /// and vice versa. PointerDown and DoubleClick resolve through the exact
    /// same lookup in `handle_event`. C-P1c proved this for Light/Camera on
    /// the card slider's own registration; C-P1d moves the LAST family off
    /// the old bespoke `_value_cells` lookup (Modifier's `object_value_cells`,
    /// deleted this phase) — this test now drives both gestures on
    /// `modifier_card`'s own slider ids, the azalea fixture's Bend "Angle"
    /// row, same shape `roughness_card_row_sweeps_full_range_and_value_box_opens_typein`
    /// already proves for Object.
    #[test]
    fn dock_numeric_cells_register_full_contract() {
        {
            let mut panel = ScenePanel::new();
            panel.open();
            panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
            let mut tree = UITree::new();
            panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

            let slider_ids: Vec<(usize, crate::slider::SliderNodeIds)> = panel
                .modifier_card
                .slider_ids
                .iter()
                .enumerate()
                .filter_map(|(pi, ids)| ids.map(|ids| (pi, ids)))
                .collect();
            assert!(!slider_ids.is_empty(), "azalea fixture must exercise at least one modifier card slider");

            for (pi, ids) in slider_ids {
                let (drag_consumed, _) = panel.handle_event(&UIEvent::PointerDown {
                    node_id: ids.track,
                    pos: Vec2::new(ids.track_rect.x, 0.0),
                    modifiers: crate::input::Modifiers::default(),
                });
                assert!(drag_consumed, "track {:?} must be drag-armable", ids.track);
                panel.handle_event(&UIEvent::PointerUp { node_id: Some(ids.track), pos: Vec2::ZERO });

                let is_enum = panel
                    .modifier_card
                    .param_info
                    .get(pi)
                    .and_then(|p| p.value_labels.as_ref())
                    .is_some();
                if is_enum {
                    // BUG-250: an enum cell's click-to-change is single-click
                    // cycle/dropdown; double-click must NOT open the numeric
                    // type-in (registration parity is drag + enum-click).
                    let (typein_consumed, actions) = panel.handle_event(&UIEvent::DoubleClick {
                        node_id: ids.value_text,
                        pos: Vec2::ZERO,
                        modifiers: crate::input::Modifiers::default(),
                    });
                    assert!(
                        !matches!(actions.as_slice(), [PanelAction::SceneSetupBeginNumericTextInput { .. }]),
                        "enum cell {ids:?} must not open numeric type-in, got {actions:?}"
                    );
                    let _ = typein_consumed;

                    let (click_consumed, actions) = panel.handle_event(&UIEvent::Click {
                        node_id: ids.value_text,
                        pos: Vec2::ZERO,
                        modifiers: crate::input::Modifiers::default(),
                    });
                    assert!(click_consumed, "enum value cell {:?} must dispatch on click", ids.value_text);
                    assert!(
                        matches!(
                            actions.as_slice(),
                            [PanelAction::ParamSnapshot(..), PanelAction::ParamChanged(..), PanelAction::ParamCommit(..)]
                                | [PanelAction::ParamEnumDropdown { .. }]
                        ),
                        "enum click must cycle (trio) or open the dropdown, got {actions:?}"
                    );
                } else {
                    let (typein_consumed, actions) = panel.handle_event(&UIEvent::DoubleClick {
                        node_id: ids.value_text,
                        pos: Vec2::ZERO,
                        modifiers: crate::input::Modifiers::default(),
                    });
                    assert!(typein_consumed, "value cell {:?} must also open type-in (registration parity)", ids.value_text);
                    assert!(
                        matches!(actions.as_slice(), [PanelAction::SceneSetupBeginNumericTextInput { .. }]),
                        "double-click must emit SceneSetupBeginNumericTextInput, got {actions:?}"
                    );
                }
            }
        }
    }

    /// C-P1c supersedes the old P4/D9 dropdown-on-click mechanism for Light:
    /// Mode/Cast Shadows/Shadow Softness now build through the shared card
    /// row core (`ParamInfo.value_labels`, proven by
    /// `light_enum_rows_carry_value_labels_on_the_card_row`) — the SAME
    /// drag/type-in interaction every other card row uses, not a bespoke
    /// click-to-open dropdown. C-P1d converted Modifier's Axis rows onto the
    /// same path, so `PanelAction::SceneSetupEnumClicked` now has NO
    /// producer left anywhere in this panel (`object_enum_cells`, its last
    /// source, is deleted) — a click on the Light card row's own value-text
    /// cell must NOT emit it, same as it never could for any other family.
    #[test]
    fn light_enum_row_click_does_not_emit_scene_setup_enum_clicked() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let softness_ids = panel.light_card.slider_ids[LIGHT_ROW_SHADOW_SOFTNESS]
            .expect("Shadow Softness renders a card slider");
        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: softness_ids.value_text,
            pos: Vec2::ZERO,
            modifiers: crate::input::Modifiers::default(),
        });
        assert!(
            !consumed || !matches!(actions.as_slice(), [PanelAction::SceneSetupEnumClicked { .. }]),
            "the Light card row's enum value must never dispatch the old dropdown action, got {actions:?}"
        );
    }

    /// P4, D10: the rotation row displays degrees, not the stored radians —
    /// the conversion happens ONLY at this display boundary (the underlying
    /// `RowValue.value` stays radians). C-P1b: the converted card row's own
    /// `is_angle` flag (set structurally by `build_object_card_row`, not
    /// derived from `param_id`) drives the conversion now — `format_param_value`'s
    /// is_angle branch rounds to whole degrees (`"{:.0}°"`), not the old
    /// bespoke triplet formatter's one-decimal `"{:.1}°"`.
    #[test]
    fn rotation_row_displays_degrees_not_radians() {
        let mut vm = azalea_shaped_vm();
        if let ObjectRowVm::Known(row) = &mut vm.objects[0]
            && let Some(t) = &mut row.transform
        {
            t.rot.0.value.value = std::f32::consts::FRAC_PI_2; // 90°
        }
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let rot_x_row = panel
            .object_card
            .param_info
            .get(OBJ_ROW_ROT_X)
            .expect("Rotation X is a wired card row");
        assert!(rot_x_row.is_angle, "Rotation X's card row must set is_angle");
        let (_, rot_x_stored_value) = panel
            .object_card
            .id_map
            .get(&rot_x_row.param_id)
            .expect("Rotation X's synthesized id resolves in the id map");
        assert!((rot_x_stored_value - std::f32::consts::FRAC_PI_2).abs() < 1e-4, "stored value stays radians");
        let rot_x_ids = panel.object_card.slider_ids[OBJ_ROW_ROT_X].expect("Rotation X renders a slider");
        let text = tree.get_node(rot_x_ids.value_text).unwrap().text.clone().unwrap();
        assert_eq!(text, "90\u{00b0}", "display converts to degrees at the panel boundary");
    }

    /// P4, D10: typing "45" into a rot cell must land 0.7853981 rad (π/4) —
    /// the commit-side half of the degrees boundary. Exercised directly
    /// against `is_degrees_param` + the conversion the commit path applies,
    /// since the actual text-input session lives in `manifold-app`.
    #[test]
    fn degrees_row_type_in_parses_to_radians() {
        assert!(is_degrees_param("rot_x"));
        let typed: f32 = "45".parse().unwrap();
        let radians = typed.to_radians();
        assert!((radians - 0.7853981).abs() < 1e-5);
    }
}
