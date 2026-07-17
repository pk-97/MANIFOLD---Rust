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
use crate::drag::DragController;
use crate::input::UIEvent;
use crate::node::*;
use crate::scroll_container::{SCROLLBAR_W, ScrollContainer, ScrollbarStyle};
use crate::tree::UITree;
use manifold_foundation::LayerId;

use super::PanelAction;

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
/// rows above), so — like `KEY_ROW_BASE`/`row_key` — every object gets a
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
// Triplet rows (`build_triplet_row`) take only the FIRST offset — Y/Z cells
// key off `base_offset + 1`/`+ 2` (the cell loop's `i`), so only the X/R
// anchor needs a named constant.
const OBJ_OFF_POS_X: u64 = 2;
const OBJ_OFF_ROT_X: u64 = 5;
const OBJ_OFF_SCALE_X: u64 = 8;
const OBJ_OFF_COLOR_R: u64 = 11;
// UX-P2 (D2 of SCENE_PANEL_UX_DESIGN.md): Metallic/Roughness are now single
// `BitmapSlider` rows (`build_object_slider_row`), one key per row — the old
// `[-] value [+]` stepper's `+1`/`+2` value/plus slots are unused now, kept
// reserved (harmless) rather than renumbering every offset below.
const OBJ_OFF_METALLIC: u64 = 14;
const OBJ_OFF_ROUGHNESS: u64 = 17;
/// BUG-193 per-row "✕" remove button, on the title row next to the name.
const OBJ_OFF_REMOVE: u64 = 20;
// UX-P3a: mod-button keys, one per exposable field — offsets 22..32 (11
// slots), clear of `obj_key(row.index, OBJ_OFF_REMOVE) + 1` (the
// properties-header Duplicate button, offset 21) and under the bumped
// `OBJ_KEY_STRIDE` (44). Triplet mod buttons take the axis-0 anchor here
// and key off `+0/+1/+2` (the cell loop's `i`), same convention
// `OBJ_OFF_POS_X` etc. already use for their value cells.
const OBJ_OFF_POS_X_MOD: u64 = 22;
const OBJ_OFF_ROT_X_MOD: u64 = 25;
const OBJ_OFF_SCALE_X_MOD: u64 = 28;
const OBJ_OFF_METALLIC_MOD: u64 = 31;
const OBJ_OFF_ROUGHNESS_MOD: u64 = 32;

const fn obj_key(index: usize, offset: u64) -> u64 {
    OBJ_KEY_BASE + index as u64 * OBJ_KEY_STRIDE + offset
}

/// Stable automation name for one triplet cell (a `build_triplet_row` value
/// box), by control kind + axis — `nth` (per-object) still disambiguates
/// which object a flow means, mirroring the audio dock's `name` + `nth`
/// convention. Same fix as `fixed_row_automation_name` — a bare `text` +
/// `under_text` selector can't tell two "0.00" cells apart in this flat
/// (no per-section container) panel.
const fn triplet_cell_automation_name(base_offset: u64, axis: usize) -> Option<&'static str> {
    match (base_offset, axis) {
        (OBJ_OFF_POS_X, 0) => Some("scene_setup.object.pos_x"),
        (OBJ_OFF_POS_X, 1) => Some("scene_setup.object.pos_y"),
        (OBJ_OFF_POS_X, 2) => Some("scene_setup.object.pos_z"),
        (OBJ_OFF_ROT_X, 0) => Some("scene_setup.object.rot_x"),
        (OBJ_OFF_ROT_X, 1) => Some("scene_setup.object.rot_y"),
        (OBJ_OFF_ROT_X, 2) => Some("scene_setup.object.rot_z"),
        (OBJ_OFF_SCALE_X, 0) => Some("scene_setup.object.scale_x"),
        (OBJ_OFF_SCALE_X, 1) => Some("scene_setup.object.scale_y"),
        (OBJ_OFF_SCALE_X, 2) => Some("scene_setup.object.scale_z"),
        (OBJ_OFF_COLOR_R, 0) => Some("scene_setup.object.color_r"),
        (OBJ_OFF_COLOR_R, 1) => Some("scene_setup.object.color_g"),
        (OBJ_OFF_COLOR_R, 2) => Some("scene_setup.object.color_b"),
        _ => None,
    }
}

/// UX-P3a: stable automation name for one exposable OBJECT row's mod button
/// (triplet/slider rows, keyed the same way `triplet_cell_automation_name`/
/// `object_slider_row_automation_name` key their own value cells) — same
/// "flat panel, no per-section container, `nth` disambiguates" convention
/// every other automation name in this file follows. `None` for a row this
/// phase doesn't wire a button onto (Color's per-channel cells).
const fn mod_button_automation_name(base_offset: u64, axis: usize) -> Option<&'static str> {
    match (base_offset, axis) {
        (OBJ_OFF_POS_X_MOD, 0) => Some("scene_setup.mod.pos_x"),
        (OBJ_OFF_POS_X_MOD, 1) => Some("scene_setup.mod.pos_y"),
        (OBJ_OFF_POS_X_MOD, 2) => Some("scene_setup.mod.pos_z"),
        (OBJ_OFF_ROT_X_MOD, 0) => Some("scene_setup.mod.rot_x"),
        (OBJ_OFF_ROT_X_MOD, 1) => Some("scene_setup.mod.rot_y"),
        (OBJ_OFF_ROT_X_MOD, 2) => Some("scene_setup.mod.rot_z"),
        (OBJ_OFF_SCALE_X_MOD, 0) => Some("scene_setup.mod.scale_x"),
        (OBJ_OFF_SCALE_X_MOD, 1) => Some("scene_setup.mod.scale_y"),
        (OBJ_OFF_SCALE_X_MOD, 2) => Some("scene_setup.mod.scale_z"),
        (OBJ_OFF_METALLIC_MOD, 0) => Some("scene_setup.mod.metallic"),
        (OBJ_OFF_ROUGHNESS_MOD, 0) => Some("scene_setup.mod.roughness"),
        _ => None,
    }
}

/// UX-P3a: stable automation name for one of the four FIXED Environment/Fog
/// rows' mod button, keyed by `row_index` — same set `fixed_row_automation_name`
/// keys its value cell by (each of the four is a distinct `row_index`, so
/// unlike the object rows this doesn't need an axis).
const fn fixed_row_mod_automation_name(row_index: u64) -> Option<&'static str> {
    match row_index {
        ROW_ENV_INTENSITY => Some("scene_setup.mod.env_intensity"),
        ROW_ENV_FILL => Some("scene_setup.mod.env_fill"),
        ROW_FOG_DENSITY => Some("scene_setup.mod.fog_density"),
        ROW_FOG_HEIGHT_FALLOFF => Some("scene_setup.mod.fog_height_falloff"),
        _ => None,
    }
}

/// Stable automation name for an object-row slider's value cell
/// (metallic/roughness, UX-P2 D2).
const fn object_slider_row_automation_name(base_offset: u64) -> Option<&'static str> {
    match base_offset {
        OBJ_OFF_METALLIC => Some("scene_setup.object.metallic_value"),
        OBJ_OFF_ROUGHNESS => Some("scene_setup.object.roughness_value"),
        _ => None,
    }
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
const LIGHT_OFF_MODE_MINUS: u64 = 1;
const LIGHT_OFF_COLOR_R: u64 = 4;
const LIGHT_OFF_INTENSITY_MINUS: u64 = 7;
const LIGHT_OFF_POS_X: u64 = 10;
const LIGHT_OFF_AIM_X: u64 = 13;
const LIGHT_OFF_CAST_SHADOWS_MINUS: u64 = 16;
const LIGHT_OFF_SHADOW_SOFTNESS_MINUS: u64 = 19;
const LIGHT_OFF_LIGHT_SIZE_MINUS: u64 = 22;
/// BUG-193 per-row "✕" remove button, on the title row next to the label.
const LIGHT_OFF_REMOVE: u64 = 26;
/// UX-P3b-i: the light-name drag/rename button's own offset, replacing the
/// `LIGHT_OFF_MODE_MINUS + 100` out-of-stride hack (see the stride comment
/// above).
const LIGHT_OFF_NAME: u64 = 27;
// UX-P3b-i: mod-button keys, one per exposable field — offsets 28..38 (10
// slots, matching the doc's "intensity/pos/aim/cast_shadows/shadow_softness/
// light_size = 10 more slots" inventory). Mode and Color stay unexposable —
// Mode is a structural type switch (same reasoning `mod_button_automation_name`
// already applies to Object rows' Color, D4: display-only) and Color's
// per-channel exposure is out of scope for the same reason.
const LIGHT_OFF_INTENSITY_MOD: u64 = 28;
const LIGHT_OFF_POS_X_MOD: u64 = 29;
const LIGHT_OFF_AIM_X_MOD: u64 = 32;
const LIGHT_OFF_CAST_SHADOWS_MOD: u64 = 35;
const LIGHT_OFF_SHADOW_SOFTNESS_MOD: u64 = 36;
const LIGHT_OFF_LIGHT_SIZE_MOD: u64 = 37;

const fn light_key(index: usize, offset: u64) -> u64 {
    LIGHT_KEY_BASE + index as u64 * LIGHT_KEY_STRIDE + offset
}

/// UX-P3b-i: stable automation name for one exposable LIGHT row's mod
/// button — same convention as `mod_button_automation_name` (Object rows).
const fn light_mod_button_automation_name(base_offset: u64, axis: usize) -> Option<&'static str> {
    match (base_offset, axis) {
        (LIGHT_OFF_INTENSITY_MOD, 0) => Some("scene_setup.mod.light_intensity"),
        (LIGHT_OFF_POS_X_MOD, 0) => Some("scene_setup.mod.light_pos_x"),
        (LIGHT_OFF_POS_X_MOD, 1) => Some("scene_setup.mod.light_pos_y"),
        (LIGHT_OFF_POS_X_MOD, 2) => Some("scene_setup.mod.light_pos_z"),
        (LIGHT_OFF_AIM_X_MOD, 0) => Some("scene_setup.mod.light_aim_x"),
        (LIGHT_OFF_AIM_X_MOD, 1) => Some("scene_setup.mod.light_aim_y"),
        (LIGHT_OFF_AIM_X_MOD, 2) => Some("scene_setup.mod.light_aim_z"),
        (LIGHT_OFF_CAST_SHADOWS_MOD, 0) => Some("scene_setup.mod.light_cast_shadows"),
        (LIGHT_OFF_SHADOW_SOFTNESS_MOD, 0) => Some("scene_setup.mod.light_shadow_softness"),
        (LIGHT_OFF_LIGHT_SIZE_MOD, 0) => Some("scene_setup.mod.light_size"),
        _ => None,
    }
}

/// Stable automation name for a light row's numeric-stepper value cell —
/// `nth` (per-light) disambiguates which light a flow means, same convention
/// as `object_slider_row_automation_name`.
const fn light_numeric_row_automation_name(base_offset: u64) -> Option<&'static str> {
    match base_offset {
        LIGHT_OFF_INTENSITY_MINUS => Some("scene_setup.light.intensity_value"),
        LIGHT_OFF_MODE_MINUS => Some("scene_setup.light.mode_value"),
        LIGHT_OFF_CAST_SHADOWS_MINUS => Some("scene_setup.light.cast_shadows_value"),
        LIGHT_OFF_SHADOW_SOFTNESS_MINUS => Some("scene_setup.light.shadow_softness_value"),
        LIGHT_OFF_LIGHT_SIZE_MINUS => Some("scene_setup.light.light_size_value"),
        _ => None,
    }
}

const fn light_triplet_cell_automation_name(base_offset: u64, axis: usize) -> Option<&'static str> {
    match (base_offset, axis) {
        (LIGHT_OFF_COLOR_R, 0) => Some("scene_setup.light.color_r"),
        (LIGHT_OFF_COLOR_R, 1) => Some("scene_setup.light.color_g"),
        (LIGHT_OFF_COLOR_R, 2) => Some("scene_setup.light.color_b"),
        (LIGHT_OFF_POS_X, 0) => Some("scene_setup.light.pos_x"),
        (LIGHT_OFF_POS_X, 1) => Some("scene_setup.light.pos_y"),
        (LIGHT_OFF_POS_X, 2) => Some("scene_setup.light.pos_z"),
        (LIGHT_OFF_AIM_X, 0) => Some("scene_setup.light.aim_x"),
        (LIGHT_OFF_AIM_X, 1) => Some("scene_setup.light.aim_y"),
        (LIGHT_OFF_AIM_X, 2) => Some("scene_setup.light.aim_z"),
        _ => None,
    }
}

/// Camera-section dynamic keys (P3). The Camera section holds exactly one
/// row set per scene (unlike Objects/Lights), so — unlike `obj_key`/
/// `light_key` — no per-index stride is needed; each possible field across
/// all three camera-atom shapes gets its own fixed offset (only the ones the
/// current `CameraRowVm` variant populates are ever built in a given frame).
const CAMERA_KEY_BASE: u64 = 86_000;
const CAMERA_OFF_ORBIT_MINUS: u64 = 0;
const CAMERA_OFF_TILT_MINUS: u64 = 3;
const CAMERA_OFF_DISTANCE_MINUS: u64 = 6;
const CAMERA_OFF_FOV_MINUS: u64 = 9;
const CAMERA_OFF_POS_X: u64 = 12;
const CAMERA_OFF_YAW_MINUS: u64 = 15;
const CAMERA_OFF_PITCH_MINUS: u64 = 18;
const CAMERA_OFF_ROLL_MINUS: u64 = 21;
const CAMERA_OFF_TARGET_X: u64 = 24;
const CAMERA_OFF_LENS_FOCUS_MINUS: u64 = 27;
const CAMERA_OFF_LENS_FSTOP_MINUS: u64 = 30;
const CAMERA_OFF_LENS_SHUTTER_MINUS: u64 = 33;
const CAMERA_OFF_LENS_EXPOSURE_MINUS: u64 = 36;
// UX-P3b-i: mod-button keys, one per exposable camera field — offsets
// 39..55 (well clear of the highest value offset in use,
// `CAMERA_OFF_LENS_EXPOSURE_MINUS + 2 == 38`). Camera has no per-index
// stride (exactly one row set per scene), so — like the value offsets
// above — each field gets its own fixed offset rather than a formula.
const CAMERA_OFF_ORBIT_MOD: u64 = 39;
const CAMERA_OFF_TILT_MOD: u64 = 40;
const CAMERA_OFF_DISTANCE_MOD: u64 = 41;
const CAMERA_OFF_FOV_MOD: u64 = 42;
const CAMERA_OFF_POS_X_MOD: u64 = 43;
const CAMERA_OFF_YAW_MOD: u64 = 46;
const CAMERA_OFF_PITCH_MOD: u64 = 47;
const CAMERA_OFF_ROLL_MOD: u64 = 48;
const CAMERA_OFF_TARGET_X_MOD: u64 = 49;
const CAMERA_OFF_LENS_FOCUS_MOD: u64 = 52;
const CAMERA_OFF_LENS_FSTOP_MOD: u64 = 53;
const CAMERA_OFF_LENS_SHUTTER_MOD: u64 = 54;
const CAMERA_OFF_LENS_EXPOSURE_MOD: u64 = 55;

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
/// Stride-3 `[−] value [+]` stepper rows follow, up to 4 param slots per
/// modifier (12 offsets) — well under `MODIFIER_ROW_STRIDE`'s 20.
const MODIFIER_OFF_PARAM_BASE: u64 = 3;
/// UX-P3b-i: one mod-button offset per param slot (up to 4), placed after
/// the 3..14 value range. Collision audit: `MODIFIER_ROW_STRIDE`'s existing
/// 20-wide budget already had 5 spare offsets (15..19) — unlike
/// `LIGHT_KEY_STRIDE`, no bump was needed here to fit the 4 new offsets.
/// Only `Numeric` param rows get a mod button (`ModifierParamRowVm::Axis`
/// stays unexposable — a structural axis-selector switch, same reasoning as
/// Light's Mode row).
const MODIFIER_OFF_PARAM_MOD_BASE: u64 = 15;
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

/// UX-P3b-i: stable automation name for a modifier param row's mod button,
/// by param slot — same convention as `modifier_param_row_automation_name`.
const fn modifier_param_mod_automation_name(param_slot: usize) -> Option<&'static str> {
    match param_slot {
        0 => Some("scene_setup.mod.modifier_param0"),
        1 => Some("scene_setup.mod.modifier_param1"),
        2 => Some("scene_setup.mod.modifier_param2"),
        3 => Some("scene_setup.mod.modifier_param3"),
        _ => None,
    }
}

const fn camera_numeric_row_automation_name(offset: u64) -> Option<&'static str> {
    match offset {
        CAMERA_OFF_ORBIT_MINUS => Some("scene_setup.camera.orbit_value"),
        CAMERA_OFF_TILT_MINUS => Some("scene_setup.camera.tilt_value"),
        CAMERA_OFF_DISTANCE_MINUS => Some("scene_setup.camera.distance_value"),
        CAMERA_OFF_FOV_MINUS => Some("scene_setup.camera.fov_y_value"),
        CAMERA_OFF_YAW_MINUS => Some("scene_setup.camera.yaw_value"),
        CAMERA_OFF_PITCH_MINUS => Some("scene_setup.camera.pitch_value"),
        CAMERA_OFF_ROLL_MINUS => Some("scene_setup.camera.roll_value"),
        CAMERA_OFF_LENS_FOCUS_MINUS => Some("scene_setup.camera.lens_focus_distance_value"),
        CAMERA_OFF_LENS_FSTOP_MINUS => Some("scene_setup.camera.lens_f_stop_value"),
        CAMERA_OFF_LENS_SHUTTER_MINUS => Some("scene_setup.camera.lens_shutter_angle_value"),
        CAMERA_OFF_LENS_EXPOSURE_MINUS => Some("scene_setup.camera.lens_exposure_ev_value"),
        _ => None,
    }
}

/// UX-P3b-i: stable automation name for one exposable CAMERA row's mod
/// button — same convention as `mod_button_automation_name`/
/// `light_mod_button_automation_name`. `axis` disambiguates a triplet cell
/// (Position/Target); numeric rows always pass `0`.
const fn camera_mod_button_automation_name(base_offset: u64, axis: usize) -> Option<&'static str> {
    match (base_offset, axis) {
        (CAMERA_OFF_ORBIT_MOD, 0) => Some("scene_setup.mod.camera_orbit"),
        (CAMERA_OFF_TILT_MOD, 0) => Some("scene_setup.mod.camera_tilt"),
        (CAMERA_OFF_DISTANCE_MOD, 0) => Some("scene_setup.mod.camera_distance"),
        (CAMERA_OFF_FOV_MOD, 0) => Some("scene_setup.mod.camera_fov_y"),
        (CAMERA_OFF_POS_X_MOD, 0) => Some("scene_setup.mod.camera_pos_x"),
        (CAMERA_OFF_POS_X_MOD, 1) => Some("scene_setup.mod.camera_pos_y"),
        (CAMERA_OFF_POS_X_MOD, 2) => Some("scene_setup.mod.camera_pos_z"),
        (CAMERA_OFF_YAW_MOD, 0) => Some("scene_setup.mod.camera_yaw"),
        (CAMERA_OFF_PITCH_MOD, 0) => Some("scene_setup.mod.camera_pitch"),
        (CAMERA_OFF_ROLL_MOD, 0) => Some("scene_setup.mod.camera_roll"),
        (CAMERA_OFF_TARGET_X_MOD, 0) => Some("scene_setup.mod.camera_target_x"),
        (CAMERA_OFF_TARGET_X_MOD, 1) => Some("scene_setup.mod.camera_target_y"),
        (CAMERA_OFF_TARGET_X_MOD, 2) => Some("scene_setup.mod.camera_target_z"),
        (CAMERA_OFF_LENS_FOCUS_MOD, 0) => Some("scene_setup.mod.camera_lens_focus_distance"),
        (CAMERA_OFF_LENS_FSTOP_MOD, 0) => Some("scene_setup.mod.camera_lens_f_stop"),
        (CAMERA_OFF_LENS_SHUTTER_MOD, 0) => Some("scene_setup.mod.camera_lens_shutter_angle"),
        (CAMERA_OFF_LENS_EXPOSURE_MOD, 0) => Some("scene_setup.mod.camera_lens_exposure_ev"),
        _ => None,
    }
}

/// Per-row stepper/drag control keys — stride leaves headroom for a handful
/// of controls per row (value drag zone + minus + plus).
const KEY_ROW_BASE: u64 = 81_000;
const KEY_ROW_STRIDE: u64 = 8;
const ROW_OFF_MINUS: u64 = 0;
const ROW_OFF_VALUE: u64 = 1;
const ROW_OFF_PLUS: u64 = 2;
/// UX-P3a mod-button key for a fixed Environment/Fog row.
const ROW_OFF_MOD: u64 = 3;

const fn row_key(row: u64, offset: u64) -> u64 {
    KEY_ROW_BASE + row * KEY_ROW_STRIDE + offset
}
// Row indices for the curated P1 sliders (stable across rebuilds regardless
// of which optional rows are present, so a widget's identity never shifts
// under the user's cursor mid-drag).
const ROW_ENV_INTENSITY: u64 = 0;
const ROW_ENV_FILL: u64 = 1;
const ROW_FOG_DENSITY: u64 = 2;
const ROW_FOG_HEIGHT_FALLOFF: u64 = 3;

/// Stable automation name for one of the four fixed rows' value cell —
/// `scripts/ui-flows/` selectors key on this instead of ambiguous
/// `text`/`under_text` queries (see `build_numeric_row`'s call site).
const fn fixed_row_automation_name(row_index: u64) -> Option<&'static str> {
    match row_index {
        ROW_ENV_INTENSITY => Some("scene_setup.environment.intensity_value"),
        ROW_ENV_FILL => Some("scene_setup.environment.fill_value"),
        ROW_FOG_DENSITY => Some("scene_setup.fog.density_value"),
        ROW_FOG_HEIGHT_FALLOFF => Some("scene_setup.fog.height_falloff_value"),
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
const VALUE_W: f32 = 70.0;
/// UX-P2 (D4/D7 of SCENE_PANEL_UX_DESIGN.md): the color row's live swatch —
/// the ONE new style constant that phase's §4 negative gate allowed. Sized
/// to sit inside a `ROW_H` row with visible margin top/bottom, echoing the
/// audio dock's identity swatch (`audio_setup_panel.rs`'s `SWATCH_W`) at a
/// square, not that dock's send-row proportions.
/// UX-P3a (D9's swatch polish, sizing amendment): bumped 14→20 — "reads as
/// a color chip, not a checkbox." The hairline border already existed
/// (`border_width: 1.0` in `build_color_row`); only the size needed fixing.
const SWATCH_W: f32 = 20.0;
/// UX-P3a: the mod-button widget's fixed width, reserved out of every
/// exposable row's usable width (same "reserved slot" convention as the
/// outliner's `EyeSlot`). Square, `ROW_H` tall.
const MOD_BTN_W: f32 = 18.0;
/// Gap between a row's numeric controls and its mod button.
const MOD_BTN_GAP: f32 = 3.0;

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

/// UX-P2 (D2): which of the panel's two persistent slider widgets a call to
/// `build_object_slider_row` is targeting — selects the `SliderDragState` +
/// row-presence flag pair, same idea as `layer_header.rs`'s per-row `index`
/// but degenerate to two named slots (Metallic/Roughness are the only two
/// bounded material scalars the VM exposes today; a third would need this
/// generalized to a `Vec`, per D2's own "any future 0..1 material scalar").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ObjSlider {
    Metallic,
    Roughness,
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
    Dimmed,
}

#[derive(Clone, Debug, PartialEq)]
pub enum EnvironmentRowVm {
    /// Importer shape (switch_texture selecting Softbox/HDRI) — Mode is
    /// shown as a static chip in P1 (toggling it is a P2+ affordance; the
    /// value is legible, just not yet a control here).
    Importer { mode_is_hdri: bool, intensity: RowValue, fill: RowValue, hdri_file: String },
    Bare { intensity: RowValue, fill: RowValue },
    /// Some other producer wired into `envmap` — honest custom row, no
    /// controls (D3).
    Custom,
    /// Unwired — the "Add environment" empty row.
    None,
}

#[derive(Clone, Debug, PartialEq)]
pub enum AtmosphereRowVm {
    Wired { density: RowValue, height_falloff: RowValue },
    /// Unwired — the "Add fog" empty row.
    None,
}

/// One `node.transform_3d`'s "3 compact triplets" (D4): Position/Rotation/
/// Scale, each X/Y/Z a [`RowValue`].
#[derive(Clone, Debug, PartialEq)]
pub struct TransformRowVm {
    pub pos: (RowValue, RowValue, RowValue),
    pub rot: (RowValue, RowValue, RowValue),
    pub scale: (RowValue, RowValue, RowValue),
}

/// The Objects section's material quick-knob row (D3/D4): base color always,
/// metallic/roughness only for `pbr_material` (phong/unlit/cel don't have
/// that param — "the atom's own params otherwise").
#[derive(Clone, Debug, PartialEq)]
pub enum ObjectMaterialVm {
    Pbr { color: (RowValue, RowValue, RowValue), metallic: RowValue, roughness: RowValue },
    Other { color: (RowValue, RowValue, RowValue) },
    /// No material resolved on this object.
    None,
}

/// One editable param row inside a modifier's own param set (D6: "the atom's
/// own params (amount/axis/center …) as ordinary editable rows"). `label` is
/// the primitive's own param label, transcribed by `state_sync` (this crate
/// can't depend on `manifold-renderer`'s `ParamDef`, same DTO-boundary
/// convention as `EnvironmentRowVm::mode_is_hdri`). `Axis` covers
/// Bend/Twist/Taper's own X/Y/Z selector — the same `EnumRowValue` stepper
/// Lights already use, never a new widget kind.
#[derive(Clone, Debug, PartialEq)]
pub enum ModifierParamRowVm {
    Numeric { label: &'static str, row: RowValue },
    Axis { label: &'static str, row: EnumRowValue },
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

/// A stepper row whose value is an enum index rather than a raw float — the
/// same `[label] [−] value [+]` shape as [`RowValue`]'s numeric steppers, but
/// the value cell shows `labels[value.round() as usize]` instead of a
/// decimal (D4: `shadow_softness` "as the same stepper the importer card
/// uses"). `row.min`/`row.max` are `0.0`/`labels.len() - 1` and the stepper
/// delta is always `1.0` (round to the next label) — never the 0.05 numeric
/// nudge. This crate can't depend on `manifold-renderer`'s `LIGHT_MODES`/
/// `SHADOW_SOFTNESS_LABELS` constants, so `labels` is transcribed by
/// `state_sync` (the same DTO-boundary convention as
/// `EnvironmentRowVm::mode_is_hdri`).
#[derive(Clone, Debug, PartialEq)]
pub struct EnumRowValue {
    pub row: RowValue,
    pub labels: Vec<&'static str>,
}

/// One light row's full editable surface (D3/D4): mode, color, intensity,
/// pos/aim, cast_shadows, shadow_softness, and light_size — the last shown
/// as a sub-row beneath shadow_softness but ALWAYS present and editable
/// (parameter dependency, not conditional UI — `feedback_no_conditionally_visible_ui`).
#[derive(Clone, Debug, PartialEq)]
pub struct LightKnownRow {
    pub index: usize,
    pub node_doc_id: u32,
    /// P5: the light's editable display name (NEW — lights didn't have one
    /// before this design). Double-click opens the same rename UX as an
    /// object's name.
    pub name: String,
    pub mode: EnumRowValue,
    pub color: (RowValue, RowValue, RowValue),
    pub intensity: RowValue,
    pub pos: (RowValue, RowValue, RowValue),
    pub aim: (RowValue, RowValue, RowValue),
    /// A 2-label (`Off`/`On`) enum stepper over the raw [0,1] threshold —
    /// same shape as `mode`/`shadow_softness`, not a bespoke toggle widget.
    pub cast_shadows: EnumRowValue,
    pub shadow_softness: EnumRowValue,
    pub light_size: RowValue,
}

/// One Lights-section row.
#[derive(Clone, Debug, PartialEq)]
pub enum LightRowVm {
    Known(Box<LightKnownRow>),
    /// Producer wasn't `node.light` — honest custom row (D3).
    Custom { index: usize },
}

/// `node.camera_lens`'s four params (D3: "the lens node's own row beneath").
#[derive(Clone, Debug, PartialEq)]
pub struct LensRowVm {
    pub focus_distance: RowValue,
    pub f_stop: RowValue,
    pub shutter_angle: RowValue,
    pub exposure_ev: RowValue,
}

#[derive(Clone, Debug, PartialEq)]
pub struct OrbitCameraRowVm {
    pub orbit: RowValue,
    pub tilt: RowValue,
    pub distance: RowValue,
    pub fov_y: RowValue,
    pub lens: Option<LensRowVm>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FreeCameraRowVm {
    pub pos: (RowValue, RowValue, RowValue),
    pub yaw: RowValue,
    pub pitch: RowValue,
    pub roll: RowValue,
    pub fov_y: RowValue,
    pub lens: Option<LensRowVm>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct LookAtCameraRowVm {
    pub pos: (RowValue, RowValue, RowValue),
    pub target: (RowValue, RowValue, RowValue),
    pub fov_y: RowValue,
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

/// UX-P3a click-time context for one row's mod button: everything
/// `PanelAction::SceneSetupExposeParam` needs to build the
/// `ToggleNodeParamExposeCommand` that exposes this inner param onto the
/// layer's generator card, named `<ObjectName> · <ParamLabel>` (D8).
/// `object_label`/`param_label` are the panel's OWN row-label strings
/// (e.g. "Azalea" / "Roughness"), captured at build time — this crate can't
/// depend on `manifold-renderer`'s `ParamDef`, same DTO-boundary convention
/// every other transcribed label in this file already follows.
#[derive(Clone, Debug)]
struct ModExposeCtx {
    addr: RowAddr,
    object_label: String,
    param_label: String,
    min: f32,
    max: f32,
    /// The row's live value at click time — used as the appended card
    /// binding's `default_value`. Not the primitive's TRUE declared
    /// default (this crate has no registry access to look that up); "expose
    /// at its current value" is the honest, defensible reading of a
    /// first-click expose, and `default_value` is display/reset-only
    /// downstream (never read by the modulation/automation write path).
    value: f32,
    /// `true` for the three `transform_3d.rot_*` params — the ONLY scene
    /// rows stored in radians but shown in degrees (`is_degrees_param`).
    /// Flows onto the appended binding's `is_angle` so the card slider
    /// keeps the same degrees presentation the panel itself uses.
    is_angle: bool,
}

/// A value-label drag session (D7 gesture: "ride Fog density with the
/// mouse") — same pointer-down-arms/drag-computes/release-clears shape as
/// `AudioSetupPanel`'s gain-stepper calibration drag.
#[derive(Clone, Debug)]
struct ValueDrag {
    addr: RowAddr,
    start_x: f32,
    start_value: f32,
    min: f32,
    max: f32,
    /// Shift held at drag-start (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md P4,
    /// D8): the applied per-pixel delta is multiplied by 0.1 for the life of
    /// the drag — the performer's precision-landing gesture (e.g. Fog
    /// density to exactly 0.42, mid-set, one hand).
    fine: bool,
}

/// One numeric row's interactive node ids, set by `build_numeric_row` when
/// the row is live (driven rows leave all three `None` — no steppers).
/// Imperative `tree.add_*` calls (unlike the declarative `ChromeHost`/`View`
/// chrome) don't register a key→NodeId lookup of their own, so — same
/// convention `AudioSetupPanel::SendRowIds` uses — the panel stores each
/// dynamic control's id directly instead of re-deriving it from a key.
#[derive(Clone, Copy, Default)]
struct RowIds {
    minus: Option<NodeId>,
    value: Option<NodeId>,
    plus: Option<NodeId>,
}

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
    /// Indexed by the `ROW_*` constants.
    row_ids: [RowIds; 4],
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
    /// Every Objects-row drag-armable value cell built this frame: triplet
    /// axes (pos/rot/scale/color) + the metallic/roughness value boxes.
    /// Rebuilt fresh every `build_nodes` call — Objects is a variable-length
    /// list, so (unlike the fixed `row_ids` above) there's no fixed index
    /// table to key by; PointerDown/Drag look the control up directly here.
    object_value_cells: Vec<(NodeId, RowValue)>,
    /// Every Objects-row stepper (+/-) built this frame, with its fixed step
    /// delta (mirrors `stepper_hit` for the fixed rows above).
    object_steppers: Vec<(NodeId, RowValue, f32)>,
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
    /// P4 (D9): every Objects-scoped enum value cell built this frame
    /// (modifier Axis rows) — `(cell_node_id, row, labels)`. A cell here
    /// with `labels.len() >= 3` opens the dropdown on click; a 2-label cell
    /// stays a stepper (its `[-]/[+]` cycle lives in `object_steppers`, same
    /// as every other enum row — this vector only exists to route the
    /// VALUE cell's own click, which the `[-]/[+]` steppers don't cover).
    object_enum_cells: Vec<(NodeId, RowValue, Vec<&'static str>)>,
    /// P3 Lights-row drag-armable value cells (color/pos/aim triplets) —
    /// same convention as `object_value_cells`.
    light_value_cells: Vec<(NodeId, RowValue)>,
    /// P3 Lights-row steppers (+/-): both plain numeric (intensity,
    /// light_size) and enum (mode, cast_shadows, shadow_softness — delta
    /// `1.0`, value clamped to the label range) share this one vector, same
    /// as `object_steppers`.
    light_steppers: Vec<(NodeId, RowValue, f32)>,
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
    /// P4 (D9): every Lights-scoped enum value cell built this frame (mode /
    /// cast_shadows / shadow_softness) — same convention as
    /// `object_enum_cells`.
    light_enum_cells: Vec<(NodeId, RowValue, Vec<&'static str>)>,
    /// P3 Camera-row drag-armable value cells — same convention as
    /// `object_value_cells`, but the Camera section holds exactly one row
    /// set (no per-index list).
    camera_value_cells: Vec<(NodeId, RowValue)>,
    /// P3 Camera-row steppers (+/-) — same convention as `object_steppers`.
    camera_steppers: Vec<(NodeId, RowValue, f32)>,
    panel_rect: Rect,
    drag: DragController<ValueDrag>,
    /// The layer_id a drag targets — captured at PointerDown so `on_event`
    /// doesn't need to re-read `self.state` (which may rebuild mid-drag on
    /// an unrelated `configure`, per D1 "no staleness": the drag itself
    /// still targets the layer it started on).
    drag_layer_id: Option<LayerId>,
    /// UX-P2 (D2): the Metallic slider's widget infra — shared `BitmapSlider`
    /// drag machinery, same pattern as `layer_header.rs`'s gain slider.
    /// Persists across rebuilds (unlike the `Vec`-based cell tables above)
    /// so a drag survives the panel's per-frame tree rebuild; `set_ids` is
    /// called fresh every frame the row renders, which keeps its
    /// `track_rect` current without disturbing `drag`'s active state.
    metallic_slider: crate::slider::SliderDragState,
    /// The Metallic row's write address for the currently-built frame, or
    /// `None` when the selected object has no PBR material this frame — the
    /// signal `build_nodes` uses to `clear()` the slider above instead of
    /// leaving it pointing at a stale (possibly since-reused) `NodeId`.
    metallic_slider_row: Option<RowValue>,
    /// Same as `metallic_slider`, for Roughness.
    roughness_slider: crate::slider::SliderDragState,
    /// Same as `metallic_slider_row`, for Roughness.
    roughness_slider_row: Option<RowValue>,
    /// UX-P3a (SCENE_PANEL_UX_DESIGN.md): every LIVE (non-driven) mod-button
    /// node built this frame, paired with the click-time context
    /// `ToggleNodeParamExposeCommand` needs. One vector across every row
    /// family (numeric/triplet/slider) — the button's meaning is uniform
    /// regardless of which builder drew it, so one lookup covers all of
    /// them (same "one vector, one click arm" shape as `object_value_cells`
    /// et al.). A driven row's button is drawn dimmed and NOT pushed here
    /// (EyeSlot's Live/Dimmed convention) — it reads as present but inert,
    /// never absent (`feedback_no_conditionally_visible_ui`).
    mod_button_ids: Vec<(NodeId, ModExposeCtx)>,
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
            row_ids: [RowIds::default(); 4],
            add_object_id: None,
            add_light_id: None,
            import_model_id: None,
            selection: std::collections::HashMap::new(),
            outliner_row_ids: Vec::new(),
            outliner_eye_ids: Vec::new(),
            object_value_cells: Vec::new(),
            object_steppers: Vec::new(),
            object_name_ids: Vec::new(),
            object_remove_ids: Vec::new(),
            object_duplicate_ids: Vec::new(),
            modifier_remove_ids: Vec::new(),
            modifier_move_ids: Vec::new(),
            add_modifier_button_id: None,
            object_enum_cells: Vec::new(),
            light_value_cells: Vec::new(),
            light_steppers: Vec::new(),
            light_remove_ids: Vec::new(),
            light_name_ids: Vec::new(),
            light_enum_cells: Vec::new(),
            camera_value_cells: Vec::new(),
            camera_steppers: Vec::new(),
            panel_rect: Rect::new(0.0, 0.0, 0.0, 0.0),
            drag: DragController::new(),
            drag_layer_id: None,
            metallic_slider: crate::slider::SliderDragState::with_range(0.0, 1.0, false),
            metallic_slider_row: None,
            roughness_slider: crate::slider::SliderDragState::with_range(0.0, 1.0, false),
            roughness_slider_row: None,
            mod_button_ids: Vec::new(),
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
        self.row_ids = [RowIds::default(); 4];
        self.add_object_id = None;
        self.add_light_id = None;
        self.import_model_id = None;
        self.outliner_row_ids.clear();
        self.outliner_eye_ids.clear();
        self.object_value_cells.clear();
        self.object_steppers.clear();
        self.object_name_ids.clear();
        self.object_remove_ids.clear();
        self.object_duplicate_ids.clear();
        self.modifier_remove_ids.clear();
        self.modifier_move_ids.clear();
        self.add_modifier_button_id = None;
        self.object_enum_cells.clear();
        self.light_value_cells.clear();
        self.light_steppers.clear();
        self.light_remove_ids.clear();
        self.light_name_ids.clear();
        self.light_enum_cells.clear();
        self.camera_value_cells.clear();
        self.camera_steppers.clear();
        self.mod_button_ids.clear();
        // UX-P2: the row-presence flags below are cleared here and only set
        // back by `build_object_slider_row` if the selected object still has
        // a PBR material this frame; the end-of-`build_nodes` check clears
        // the slider `ids` (not the drag machinery) when the row didn't
        // rebuild, so a stale `NodeId` from a since-reused tree slot can
        // never resolve as a hit.
        self.metallic_slider_row = None;
        self.roughness_slider_row = None;

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

        // UX-P2: a slider whose row didn't rebuild this frame (selection
        // moved off a PBR object, or off Objects entirely) has a stale
        // `ids` pointing at a `NodeId` slot the fresh tree may have handed
        // to something else — `clear()` drops `ids` to `None` so
        // `try_start_drag` can never false-hit it. A slider whose row DID
        // rebuild got a fresh `set_ids` already, so this is a no-op for it —
        // and since a live drag keeps rebuilding at the same row every
        // frame (the user's mouse is busy, selection can't move mid-scrub),
        // `clear()` here never interrupts an in-progress drag.
        if self.metallic_slider_row.is_none() {
            self.metallic_slider.clear();
        }
        if self.roughness_slider_row.is_none() {
            self.roughness_slider.clear();
        }

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
            tree, inner_x, inner_w, cy, "\u{1F4F7} Camera", SceneSelection::Camera, selected, EyeSlot::Dimmed,
        );
        cy = self.build_outliner_row(
            tree, inner_x, inner_w, cy, "\u{1F30D} World", SceneSelection::World, selected, EyeSlot::Dimmed,
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
                        EyeSlot::Dimmed,
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
    /// affordance slot (D5) — a live eye toggle (`EyeSlot::Live`) or a
    /// dimmed, non-interactive eye glyph (`EyeSlot::Dimmed`), always at the
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
            EyeSlot::Dimmed => {
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
        }
        cy + ROW_H
    }

    /// A non-selectable outliner row (`Custom` object/light rows, D12/D3 —
    /// no addressable node id) rendered in the SAME `[name | dimmed eye]`
    /// shape `build_outliner_row` uses, minus the click target — the row
    /// template is uniform across every row regardless of interactivity
    /// (D5).
    fn build_outliner_row_static(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w - STEP_W, ROW_H, label, label_style());
        tree.add_label(
            Some(self.content_parent),
            inner_x + inner_w - STEP_W,
            cy,
            STEP_W,
            ROW_H,
            "\u{1F441}",
            driven_label_style(),
        );
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
                self.build_object_properties_body(tree, inner_x, inner_w, cy, row, &vm.layer_id)
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
        layer_id: &LayerId,
    ) -> f32 {
        let index = row.index;
        let obj_label = row.name.as_str();
        if let Some(t) = &row.transform {
            cy = self.build_triplet_row(
                tree, inner_x, inner_w, cy, "Position", &t.pos, index, OBJ_OFF_POS_X,
                Some(obj_label), Some(OBJ_OFF_POS_X_MOD),
            );
            cy = self.build_triplet_row(
                tree, inner_x, inner_w, cy, "Rotation", &t.rot, index, OBJ_OFF_ROT_X,
                Some(obj_label), Some(OBJ_OFF_ROT_X_MOD),
            );
            cy = self.build_triplet_row(
                tree, inner_x, inner_w, cy, "Scale", &t.scale, index, OBJ_OFF_SCALE_X,
                Some(obj_label), Some(OBJ_OFF_SCALE_X_MOD),
            );
        }
        match &row.material {
            ObjectMaterialVm::Pbr { color, metallic, roughness } => {
                cy = self.build_color_row(tree, inner_x, inner_w, cy, color, index, OBJ_OFF_COLOR_R);
                cy = self.build_object_slider_row(
                    tree, inner_x, inner_w, cy, "Metallic", metallic, layer_id, OBJ_OFF_METALLIC, ObjSlider::Metallic,
                    index, obj_label, OBJ_OFF_METALLIC_MOD,
                );
                cy = self.build_object_slider_row(
                    tree, inner_x, inner_w, cy, "Roughness", roughness, layer_id, OBJ_OFF_ROUGHNESS, ObjSlider::Roughness,
                    index, obj_label, OBJ_OFF_ROUGHNESS_MOD,
                );
            }
            ObjectMaterialVm::Other { color } => {
                cy = self.build_color_row(tree, inner_x, inner_w, cy, color, index, OBJ_OFF_COLOR_R);
            }
            ObjectMaterialVm::None => {
                tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "No material", label_style());
                cy += ROW_H;
            }
        }
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Modifiers", label_style());
        cy += ROW_H;
        if row.modifiers_addable {
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
                    obj_label,
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
    fn build_light_properties_body(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        row: &LightKnownRow,
    ) -> f32 {
        let index = row.index;
        let obj_label = row.name.as_str();
        cy = self.build_light_enum_row(
            tree, inner_x, inner_w, cy, "Mode", &row.mode, index, LIGHT_OFF_MODE_MINUS, None, None,
        );
        cy = self.build_light_triplet_row(
            tree, inner_x, inner_w, cy, "Color", &row.color, index, LIGHT_OFF_COLOR_R, None, None,
        );
        cy = self.build_light_numeric_row(
            tree, inner_x, inner_w, cy, "Intensity", &row.intensity, index, LIGHT_OFF_INTENSITY_MINUS,
            obj_label, LIGHT_OFF_INTENSITY_MOD,
        );
        cy = self.build_light_triplet_row(
            tree, inner_x, inner_w, cy, "Position", &row.pos, index, LIGHT_OFF_POS_X,
            Some(obj_label), Some(LIGHT_OFF_POS_X_MOD),
        );
        cy = self.build_light_triplet_row(
            tree, inner_x, inner_w, cy, "Aim", &row.aim, index, LIGHT_OFF_AIM_X,
            Some(obj_label), Some(LIGHT_OFF_AIM_X_MOD),
        );
        cy = self.build_light_enum_row(
            tree, inner_x, inner_w, cy, "Cast Shadows", &row.cast_shadows, index, LIGHT_OFF_CAST_SHADOWS_MINUS,
            Some(obj_label), Some(LIGHT_OFF_CAST_SHADOWS_MOD),
        );
        cy = self.build_light_enum_row(
            tree, inner_x, inner_w, cy, "Shadow Softness", &row.shadow_softness, index, LIGHT_OFF_SHADOW_SOFTNESS_MINUS,
            Some(obj_label), Some(LIGHT_OFF_SHADOW_SOFTNESS_MOD),
        );
        cy = self.build_light_numeric_row(
            tree,
            inner_x + PAD,
            inner_w - PAD,
            cy,
            "Light Size",
            &row.light_size,
            index,
            LIGHT_OFF_LIGHT_SIZE_MINUS,
            obj_label,
            LIGHT_OFF_LIGHT_SIZE_MOD,
        );
        cy + ROW_GAP
    }

    /// World properties: Environment + Fog (v1's sections, unchanged bodies
    /// — carried forward per D7's "World → Environment + Fog sections").
    fn build_world_properties(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        mut cy: f32,
        vm: &SceneSetupVm,
    ) -> f32 {
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
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Intensity", intensity, ROW_ENV_INTENSITY, "Environment");
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Fill", fill, ROW_ENV_FILL, "Environment");
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
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Intensity", intensity, ROW_ENV_INTENSITY, "Environment");
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Fill", fill, ROW_ENV_FILL, "Environment");
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
                cy = self.build_numeric_row(tree, inner_x, inner_w, cy, "Density", density, ROW_FOG_DENSITY, "Fog");
                cy = self.build_numeric_row(
                    tree,
                    inner_x,
                    inner_w,
                    cy,
                    "Height Falloff",
                    height_falloff,
                    ROW_FOG_HEIGHT_FALLOFF,
                    "Fog",
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

    /// UX-P3a (D8/D9 of SCENE_PANEL_UX_DESIGN.md, sizing amendment): the mod
    /// button for one exposable row. Draws at the row's reserved right-edge
    /// slot (`MOD_BTN_W`). A driven row gets the dimmed, non-interactive
    /// variant and is NOT pushed into `mod_button_ids` (EyeSlot's
    /// Live/Dimmed convention — the slot is always drawn, never absent).
    /// `object_label`/`param_label` feed the exposure's card name
    /// (`<ObjectName> · <ParamLabel>`, D8) — the panel's own row-label
    /// strings, not re-derived from the primitive registry.
    #[allow(clippy::too_many_arguments)]
    #[allow(clippy::too_many_arguments)]
    fn build_mod_button(
        &mut self,
        tree: &mut UITree,
        x: f32,
        cy: f32,
        row: &RowValue,
        object_label: &str,
        param_label: &str,
        is_angle: bool,
        key: u64,
        automation_name: Option<&'static str>,
    ) {
        let id = tree.add_button_keyed(
            Some(self.content_parent),
            x,
            cy,
            MOD_BTN_W,
            ROW_H,
            mod_btn_style(row.exposed && !row.driven),
            "\u{223F}",
            key,
        );
        if let Some(name) = automation_name {
            tree.set_name(id, name);
        }
        if !row.driven {
            self.mod_button_ids.push((
                id,
                ModExposeCtx {
                    addr: row.addr.clone(),
                    object_label: object_label.to_string(),
                    param_label: param_label.to_string(),
                    min: row.min,
                    max: row.max,
                    value: row.value,
                    is_angle,
                },
            ));
        }
    }

    /// One `[label]  [−] value [＋]` numeric row. Driven rows (D4) render
    /// with no interactive steppers and a dimmed "driven" badge — the panel
    /// never fights the graph.
    fn build_numeric_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &RowValue,
        row_index: u64,
        object_label: &str,
    ) -> f32 {
        // UX-P3a: reserve the mod-button slot out of the row's usable width
        // before laying out the rest — the button always sits at the far
        // right edge regardless of which branch below runs.
        let mod_x = inner_x + inner_w - MOD_BTN_W;
        let inner_w = inner_w - MOD_BTN_W - MOD_BTN_GAP;
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let slot = &mut self.row_ids[row_index as usize];
        if row.driven {
            tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                inner_w - LABEL_W,
                ROW_H,
                &format!("{:.2} (driven)", row.value),
                driven_label_style(),
            );
            *slot = RowIds::default();
            self.build_mod_button(
                tree, mod_x, cy, row, object_label, label, false, row_key(row_index, ROW_OFF_MOD),
                fixed_row_mod_automation_name(row_index),
            );
            return cy + ROW_H;
        }
        let step_x = inner_x + inner_w - VALUE_W - STEP_W * 2.0;
        let minus = tree.add_button_keyed(
            Some(self.content_parent),
            step_x,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2212}",
            row_key(row_index, ROW_OFF_MINUS),
        );
        // A real interactive widget, not a plain label: `UITree::hit_test`
        // only ever returns `INTERACTIVE`-flagged nodes (it skips a bare
        // label and falls through to whatever's behind it — the panel
        // background, in this dock), so the value cell must carry the flag
        // itself to be a legitimate pointer-down/drag target. Styled to read
        // as a drag zone, not a push-button (no press-style feedback beyond
        // the hover fill already in `drag_value_style`).
        let value = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W,
            cy,
            VALUE_W,
            ROW_H,
            drag_value_style(),
            &format!("{:.2}", row.value),
            row_key(row_index, ROW_OFF_VALUE),
        );
        // A stable automation name (P2 fix): `text`-based selectors like
        // `{"text": "0.00", "under_text": "Fog"}` are ambiguous the moment
        // ANY other row in the panel also shows "0.00" — `under_text`
        // matches on a SHARED ancestor, not literal nesting (this panel has
        // no per-section container, every row is a flat sibling under the
        // same scroll-clip parent), so it can't disambiguate two rows that
        // both read "0.00" (the P2 Objects section's default transform/
        // color cells collided with this exact row once Objects started
        // rendering above Environment/Fog — BUG found+fixed this phase).
        // `scripts/ui-flows/scene-setup-add-fog-drag.json` was updated to
        // select by name instead.
        if let Some(name) = fixed_row_automation_name(row_index) {
            tree.set_name(value, name);
        }
        let plus = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W + VALUE_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{002B}",
            row_key(row_index, ROW_OFF_PLUS),
        );
        *slot = RowIds { minus: Some(minus), value: Some(value), plus: Some(plus) };
        self.build_mod_button(
            tree, mod_x, cy, row, object_label, label, false, row_key(row_index, ROW_OFF_MOD),
            fixed_row_mod_automation_name(row_index),
        );
        cy + ROW_H
    }

    /// A light-row `[label] [−] value [+]` numeric row, keyed into the
    /// light's own range. Lights' numeric params (intensity, light size)
    /// don't get UX-P2's slider treatment — D2 scopes sliders to the
    /// Objects material rows this phase; the stepper shape stays here.
    /// UX-P3b-i: `object_label` (the light's own name) + `mod_offset` add
    /// the same mod-button parity Object rows already have — always present
    /// here (both call sites, Intensity and Light Size, are in the doc's
    /// exposable-slot inventory), so unlike `build_triplet_row`'s
    /// `Option<u64>` this takes a bare offset, mirroring
    /// `build_object_slider_row`'s `mod_key_offset: u64`.
    #[allow(clippy::too_many_arguments)]
    fn build_light_numeric_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &RowValue,
        index: usize,
        base_offset: u64,
        object_label: &str,
        mod_offset: u64,
    ) -> f32 {
        let mod_x = inner_x + inner_w - MOD_BTN_W;
        let inner_w = inner_w - MOD_BTN_W - MOD_BTN_GAP;
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        if row.driven {
            tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                inner_w - LABEL_W,
                ROW_H,
                &format!("{:.2} (driven)", row.value),
                driven_label_style(),
            );
            self.build_mod_button(
                tree, mod_x, cy, row, object_label, label, false, light_key(index, mod_offset),
                light_mod_button_automation_name(mod_offset, 0),
            );
            return cy + ROW_H;
        }
        let step_x = inner_x + inner_w - VALUE_W - STEP_W * 2.0;
        let minus_id = tree.add_button_keyed(
            Some(self.content_parent), step_x, cy, STEP_W, ROW_H, btn_style(), "\u{2212}", light_key(index, base_offset),
        );
        let value_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W,
            cy,
            VALUE_W,
            ROW_H,
            drag_value_style(),
            &format!("{:.2}", row.value),
            light_key(index, base_offset + 1),
        );
        if let Some(name) = light_numeric_row_automation_name(base_offset) {
            tree.set_name(value_id, name);
        }
        let plus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W + VALUE_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{002B}",
            light_key(index, base_offset + 2),
        );
        self.light_steppers.push((minus_id, row.clone(), -0.05));
        self.light_value_cells.push((value_id, row.clone()));
        self.light_steppers.push((plus_id, row.clone(), 0.05));
        self.build_mod_button(
            tree, mod_x, cy, row, object_label, label, false, light_key(index, mod_offset),
            light_mod_button_automation_name(mod_offset, 0),
        );
        cy + ROW_H
    }

    /// A light-row `[label] X/Y/Z` drag-value triplet — same shape as
    /// `build_triplet_row`, keyed into the light's own range. UX-P3b-i:
    /// `object_label`/`mod_base_offset` follow `build_triplet_row`'s own
    /// `Option` convention — `None` for Color (out of scope, D4), `Some` for
    /// Position/Aim.
    #[allow(clippy::too_many_arguments)]
    fn build_light_triplet_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        triplet: &(RowValue, RowValue, RowValue),
        index: usize,
        base_offset: u64,
        object_label: Option<&str>,
        mod_base_offset: Option<u64>,
    ) -> f32 {
        let reserve = mod_base_offset.is_some();
        let per_cell_reserve = if reserve { MOD_BTN_W + MOD_BTN_GAP } else { 0.0 };
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let cell_x = inner_x + LABEL_W;
        let cell_total_w = (inner_w - LABEL_W) / 3.0 - 2.0;
        let cell_w = (cell_total_w - per_cell_reserve).max(20.0);
        const AXIS: [&str; 3] = ["X", "Y", "Z"];
        for (i, row) in [&triplet.0, &triplet.1, &triplet.2].into_iter().enumerate() {
            let x = cell_x + i as f32 * (cell_total_w + 2.0);
            let mod_x = x + cell_w + MOD_BTN_GAP;
            if row.driven {
                tree.add_label(
                    Some(self.content_parent),
                    x,
                    cy,
                    cell_w,
                    ROW_H,
                    &format!("{:.2}\u{2022}", row.value),
                    driven_label_style(),
                );
                if let (Some(obj), Some(mod_base)) = (object_label, mod_base_offset) {
                    let param_label = format!("{label} {}", AXIS[i]);
                    self.build_mod_button(
                        tree, mod_x, cy, row, obj, &param_label, false, light_key(index, mod_base + i as u64),
                        light_mod_button_automation_name(mod_base, i),
                    );
                }
                continue;
            }
            let cell_id = tree.add_button_keyed(
                Some(self.content_parent),
                x,
                cy,
                cell_w,
                ROW_H,
                drag_value_style(),
                &format!("{:.2}", row.value),
                light_key(index, base_offset + i as u64),
            );
            if let Some(name) = light_triplet_cell_automation_name(base_offset, i) {
                tree.set_name(cell_id, name);
            }
            self.light_value_cells.push((cell_id, row.clone()));
            if let (Some(obj), Some(mod_base)) = (object_label, mod_base_offset) {
                let param_label = format!("{label} {}", AXIS[i]);
                self.build_mod_button(
                    tree, mod_x, cy, row, obj, &param_label, false, light_key(index, mod_base + i as u64),
                    light_mod_button_automation_name(mod_base, i),
                );
            }
        }
        cy + ROW_H
    }

    /// A light-row enum stepper (mode / cast_shadows / shadow_softness):
    /// same `[label] [−] value [+]` shape as `build_light_numeric_row`, but
    /// the value cell shows `labels[value.round() as usize]` and the
    /// stepper delta is always `1.0` (round to the next label). Not
    /// drag-armable — a label index isn't a continuous quantity to scrub,
    /// so unlike numeric/triplet cells this value cell isn't pushed into
    /// `light_value_cells`. It IS pushed into `light_enum_cells` (P4, D9):
    /// a click on a 3+-label row (e.g. `shadow_softness`) opens the
    /// dropdown; a 2-label row (`mode`, `cast_shadows`) stays a stepper —
    /// the `[-]/[+]` buttons above already cycle it either way.
    /// UX-P3b-i: `object_label`/`mod_offset` follow `build_triplet_row`'s
    /// `Option` convention — `None` for Mode (a structural type switch, not
    /// a modulatable scalar), `Some` for Cast Shadows/Shadow Softness (both
    /// labeled steppers over an underlying continuous/threshold param, same
    /// as Object's Metallic/Roughness sliders).
    #[allow(clippy::too_many_arguments)]
    fn build_light_enum_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        enum_row: &EnumRowValue,
        index: usize,
        base_offset: u64,
        object_label: Option<&str>,
        mod_offset: Option<u64>,
    ) -> f32 {
        let mod_x = inner_x + inner_w - MOD_BTN_W;
        let inner_w = if mod_offset.is_some() { inner_w - MOD_BTN_W - MOD_BTN_GAP } else { inner_w };
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let row = &enum_row.row;
        let label_text = enum_row
            .labels
            .get(row.value.round().clamp(0.0, (enum_row.labels.len().max(1) - 1) as f32) as usize)
            .copied()
            .unwrap_or("?");
        if row.driven {
            tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                inner_w - LABEL_W,
                ROW_H,
                &format!("{label_text} (driven)"),
                driven_label_style(),
            );
            if let (Some(obj), Some(mod_off)) = (object_label, mod_offset) {
                self.build_mod_button(
                    tree, mod_x, cy, row, obj, label, false, light_key(index, mod_off),
                    light_mod_button_automation_name(mod_off, 0),
                );
            }
            return cy + ROW_H;
        }
        let step_x = inner_x + inner_w - VALUE_W - STEP_W * 2.0;
        let minus_id = tree.add_button_keyed(
            Some(self.content_parent), step_x, cy, STEP_W, ROW_H, btn_style(), "\u{2212}", light_key(index, base_offset),
        );
        let value_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W,
            cy,
            VALUE_W,
            ROW_H,
            drag_value_style(),
            label_text,
            light_key(index, base_offset + 1),
        );
        if let Some(name) = light_numeric_row_automation_name(base_offset) {
            tree.set_name(value_id, name);
        }
        let plus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W + VALUE_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{002B}",
            light_key(index, base_offset + 2),
        );
        self.light_steppers.push((minus_id, row.clone(), -1.0));
        self.light_enum_cells.push((value_id, row.clone(), enum_row.labels.clone()));
        self.light_steppers.push((plus_id, row.clone(), 1.0));
        if let (Some(obj), Some(mod_off)) = (object_label, mod_offset) {
            self.build_mod_button(
                tree, mod_x, cy, row, obj, label, false, light_key(index, mod_off),
                light_mod_button_automation_name(mod_off, 0),
            );
        }
        cy + ROW_H
    }

    /// The Camera section (P3, D3/D4): exactly one row set, shape depending
    /// on which camera atom the trace resolved, plus the lens pass-through
    /// row when present.
    fn build_camera_section(&mut self, tree: &mut UITree, inner_x: f32, inner_w: f32, mut cy: f32, vm: &SceneSetupVm) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, inner_w, ROW_H, "Camera", section_label_style());
        cy += ROW_H;
        match &vm.camera {
            CameraRowVm::Orbit(row) => {
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "Orbit", &row.orbit, CAMERA_OFF_ORBIT_MINUS, CAMERA_OFF_ORBIT_MOD,
                );
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "Tilt", &row.tilt, CAMERA_OFF_TILT_MINUS, CAMERA_OFF_TILT_MOD,
                );
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "Distance", &row.distance, CAMERA_OFF_DISTANCE_MINUS,
                    CAMERA_OFF_DISTANCE_MOD,
                );
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "FOV", &row.fov_y, CAMERA_OFF_FOV_MINUS, CAMERA_OFF_FOV_MOD,
                );
                cy = self.build_camera_lens(tree, inner_x, inner_w, cy, &row.lens);
            }
            CameraRowVm::Free(row) => {
                cy = self.build_camera_triplet_row(
                    tree, inner_x, inner_w, cy, "Position", &row.pos, CAMERA_OFF_POS_X, CAMERA_OFF_POS_X_MOD,
                );
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "Yaw", &row.yaw, CAMERA_OFF_YAW_MINUS, CAMERA_OFF_YAW_MOD,
                );
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "Pitch", &row.pitch, CAMERA_OFF_PITCH_MINUS, CAMERA_OFF_PITCH_MOD,
                );
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "Roll", &row.roll, CAMERA_OFF_ROLL_MINUS, CAMERA_OFF_ROLL_MOD,
                );
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "FOV", &row.fov_y, CAMERA_OFF_FOV_MINUS, CAMERA_OFF_FOV_MOD,
                );
                cy = self.build_camera_lens(tree, inner_x, inner_w, cy, &row.lens);
            }
            CameraRowVm::LookAt(row) => {
                cy = self.build_camera_triplet_row(
                    tree, inner_x, inner_w, cy, "Position", &row.pos, CAMERA_OFF_POS_X, CAMERA_OFF_POS_X_MOD,
                );
                cy = self.build_camera_triplet_row(
                    tree, inner_x, inner_w, cy, "Target", &row.target, CAMERA_OFF_TARGET_X, CAMERA_OFF_TARGET_X_MOD,
                );
                cy = self.build_camera_numeric_row(
                    tree, inner_x, inner_w, cy, "FOV", &row.fov_y, CAMERA_OFF_FOV_MINUS, CAMERA_OFF_FOV_MOD,
                );
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
        cy = self.build_camera_numeric_row(
            tree, body_x, body_w, cy, "Focus Distance", &lens.focus_distance, CAMERA_OFF_LENS_FOCUS_MINUS,
            CAMERA_OFF_LENS_FOCUS_MOD,
        );
        cy = self.build_camera_numeric_row(
            tree, body_x, body_w, cy, "F-Stop", &lens.f_stop, CAMERA_OFF_LENS_FSTOP_MINUS, CAMERA_OFF_LENS_FSTOP_MOD,
        );
        cy = self.build_camera_numeric_row(
            tree, body_x, body_w, cy, "Shutter Angle", &lens.shutter_angle, CAMERA_OFF_LENS_SHUTTER_MINUS,
            CAMERA_OFF_LENS_SHUTTER_MOD,
        );
        self.build_camera_numeric_row(
            tree, body_x, body_w, cy, "Exposure (EV)", &lens.exposure_ev, CAMERA_OFF_LENS_EXPOSURE_MINUS,
            CAMERA_OFF_LENS_EXPOSURE_MOD,
        )
    }

    /// A camera-row `[label] [−] value [+]` numeric row — same shape as
    /// `build_light_numeric_row`, keyed into the fixed camera-section range
    /// (no per-index stride: exactly one Camera row set exists per frame).
    /// UX-P3b-i: `mod_offset` is a bare offset, not `Option` — every camera
    /// numeric row this phase wires (orbit/tilt/distance/fov/yaw/pitch/roll/
    /// the four lens fields) is in the doc's exposable inventory, same
    /// "always present" convention as `build_light_numeric_row`.
    #[allow(clippy::too_many_arguments)]
    fn build_camera_numeric_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &RowValue,
        base_offset: u64,
        mod_offset: u64,
    ) -> f32 {
        let mod_x = inner_x + inner_w - MOD_BTN_W;
        let inner_w = inner_w - MOD_BTN_W - MOD_BTN_GAP;
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let degrees = is_degrees_param(&row.addr.param_id);
        if row.driven {
            let text = if degrees {
                format!("{:.1}\u{00b0} (driven)", row.value.to_degrees())
            } else {
                format!("{:.2} (driven)", row.value)
            };
            tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                inner_w - LABEL_W,
                ROW_H,
                &text,
                driven_label_style(),
            );
            self.build_mod_button(
                tree, mod_x, cy, row, "Camera", label, degrees, CAMERA_KEY_BASE + mod_offset,
                camera_mod_button_automation_name(mod_offset, 0),
            );
            return cy + ROW_H;
        }
        let step_x = inner_x + inner_w - VALUE_W - STEP_W * 2.0;
        let minus_id = tree.add_button_keyed(
            Some(self.content_parent), step_x, cy, STEP_W, ROW_H, btn_style(), "\u{2212}", CAMERA_KEY_BASE + base_offset,
        );
        // D10: the committed degrees rows (orbit/tilt/yaw/pitch/roll/fov_y)
        // display `%.1f°`; storage/commit still speak radians (`row.value`).
        let display_text =
            if degrees { format!("{:.1}\u{00b0}", row.value.to_degrees()) } else { format!("{:.2}", row.value) };
        let value_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W,
            cy,
            VALUE_W,
            ROW_H,
            drag_value_style(),
            &display_text,
            CAMERA_KEY_BASE + base_offset + 1,
        );
        if let Some(name) = camera_numeric_row_automation_name(base_offset) {
            tree.set_name(value_id, name);
        }
        let plus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W + VALUE_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{002B}",
            CAMERA_KEY_BASE + base_offset + 2,
        );
        self.camera_steppers.push((minus_id, row.clone(), -0.05));
        self.camera_value_cells.push((value_id, row.clone()));
        self.camera_steppers.push((plus_id, row.clone(), 0.05));
        self.build_mod_button(
            tree, mod_x, cy, row, "Camera", label, degrees, CAMERA_KEY_BASE + mod_offset,
            camera_mod_button_automation_name(mod_offset, 0),
        );
        cy + ROW_H
    }

    /// A camera-row `[label] X/Y/Z` drag-value triplet — same shape as
    /// `build_light_triplet_row`, keyed into the fixed camera-section range.
    /// UX-P3b-i: `mod_base_offset` is a bare offset (both call sites,
    /// Position and Target, are in the doc's exposable inventory) — every
    /// axis is independently exposable, same "own mod button per cell"
    /// convention `build_triplet_row` uses for Object rows.
    fn build_camera_triplet_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        triplet: &(RowValue, RowValue, RowValue),
        base_offset: u64,
        mod_base_offset: u64,
    ) -> f32 {
        let per_cell_reserve = MOD_BTN_W + MOD_BTN_GAP;
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let cell_x = inner_x + LABEL_W;
        let cell_total_w = (inner_w - LABEL_W) / 3.0 - 2.0;
        let cell_w = (cell_total_w - per_cell_reserve).max(20.0);
        const AXIS: [&str; 3] = ["X", "Y", "Z"];
        for (i, row) in [&triplet.0, &triplet.1, &triplet.2].into_iter().enumerate() {
            let x = cell_x + i as f32 * (cell_total_w + 2.0);
            let mod_x = x + cell_w + MOD_BTN_GAP;
            let param_label = format!("{label} {}", AXIS[i]);
            if row.driven {
                tree.add_label(
                    Some(self.content_parent),
                    x,
                    cy,
                    cell_w,
                    ROW_H,
                    &format!("{:.2}\u{2022}", row.value),
                    driven_label_style(),
                );
                self.build_mod_button(
                    tree, mod_x, cy, row, "Camera", &param_label, false, CAMERA_KEY_BASE + mod_base_offset + i as u64,
                    camera_mod_button_automation_name(mod_base_offset, i),
                );
                continue;
            }
            let cell_id = tree.add_button_keyed(
                Some(self.content_parent),
                x,
                cy,
                cell_w,
                ROW_H,
                drag_value_style(),
                &format!("{:.2}", row.value),
                CAMERA_KEY_BASE + base_offset + i as u64,
            );
            self.camera_value_cells.push((cell_id, row.clone()));
            self.build_mod_button(
                tree, mod_x, cy, row, "Camera", &param_label, false, CAMERA_KEY_BASE + mod_base_offset + i as u64,
                camera_mod_button_automation_name(mod_base_offset, i),
            );
        }
        cy + ROW_H
    }

    /// One modifier-stack entry (P5/D6): display name + up/down/remove, then
    /// its own param rows. `mod_count` is the CURRENT stack length — up/down
    /// are always rendered (never conditionally hidden,
    /// `feedback_no_conditionally_visible_ui`) but only recorded as live
    /// targets when they wouldn't push past a stack boundary; clicking an
    /// inert one at the boundary is simply a no-op. UX-P3b-i: `object_label`
    /// (the owning object's name — modifiers live inside an object's own
    /// group, so the mod-button's exposure card name follows the same
    /// `<ObjectName> · <ParamLabel>` convention every other exposable row
    /// uses, not a modifier-scoped identity) threads down to
    /// `build_modifier_numeric_row`.
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
        object_label: &str,
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
        for (slot, p) in m.params.iter().enumerate() {
            cy = match p {
                // UX-P3b-i: the mod-button's `param_label` disambiguates
                // WHICH modifier's field this is (an object can carry
                // several modifiers, each with its own "Angle"/"Amount") —
                // "Bend Angle", not a bare "Angle" that would collide with
                // a Twist modifier's own numeric row on the same object.
                ModifierParamRowVm::Numeric { label, row } => self.build_modifier_numeric_row(
                    tree, param_x, param_w, cy, label, row, object_index, m.index, slot, object_label,
                    &format!("{} {label}", m.display_name),
                ),
                ModifierParamRowVm::Axis { label, row } => self.build_modifier_enum_row(
                    tree, param_x, param_w, cy, label, row, object_index, m.index, slot,
                ),
            };
        }
        cy
    }

    /// One modifier param's `[label] [−] value [＋]` row, keyed into the
    /// modifier-row range — a modifier's own params (bend angle, twist
    /// turns, axis, …) stay steppers; D2 scopes sliders to Objects material
    /// rows only.
    /// Pushed onto the SAME `object_steppers`/`object_value_cells` vectors
    /// every other Objects-section numeric row uses (those are already
    /// generic `(NodeId, RowValue, …)` lookups keyed by write address, not
    /// object identity — no separate modifier-specific drag plumbing needed).
    /// UX-P3b-i: `object_label`/`param_label` feed the mod button's
    /// exposure name — always present (every modifier `Numeric` param is in
    /// the doc's exposable inventory; only `Axis` rows are excluded, a
    /// structural switch, same reasoning as Light's Mode row).
    #[allow(clippy::too_many_arguments)]
    fn build_modifier_numeric_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &RowValue,
        object_index: usize,
        modifier_index: usize,
        param_slot: usize,
        object_label: &str,
        param_label: &str,
    ) -> f32 {
        let mod_x = inner_x + inner_w - MOD_BTN_W;
        let inner_w = inner_w - MOD_BTN_W - MOD_BTN_GAP;
        let mod_offset = MODIFIER_OFF_PARAM_MOD_BASE + param_slot as u64;
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        if row.driven {
            tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                inner_w - LABEL_W,
                ROW_H,
                &format!("{:.2} (driven)", row.value),
                driven_label_style(),
            );
            self.build_mod_button(
                tree, mod_x, cy, row, object_label, param_label, false,
                modifier_row_key(object_index, modifier_index, mod_offset),
                modifier_param_mod_automation_name(param_slot),
            );
            return cy + ROW_H;
        }
        let base = MODIFIER_OFF_PARAM_BASE + param_slot as u64 * 3;
        let step_x = inner_x + inner_w - VALUE_W - STEP_W * 2.0;
        let minus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2212}",
            modifier_row_key(object_index, modifier_index, base),
        );
        let value_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W,
            cy,
            VALUE_W,
            ROW_H,
            drag_value_style(),
            &format!("{:.2}", row.value),
            modifier_row_key(object_index, modifier_index, base + 1),
        );
        if let Some(name) = modifier_param_row_automation_name(param_slot) {
            tree.set_name(value_id, name);
        }
        let plus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W + VALUE_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{002B}",
            modifier_row_key(object_index, modifier_index, base + 2),
        );
        self.object_steppers.push((minus_id, row.clone(), -0.05));
        self.object_value_cells.push((value_id, row.clone()));
        self.object_steppers.push((plus_id, row.clone(), 0.05));
        self.build_mod_button(
            tree, mod_x, cy, row, object_label, param_label, false,
            modifier_row_key(object_index, modifier_index, mod_offset),
            modifier_param_mod_automation_name(param_slot),
        );
        cy + ROW_H
    }

    /// One modifier's Axis param row (Bend/Twist/Taper's X/Y/Z selector) —
    /// same shape as [`Self::build_light_enum_row`], keyed into the
    /// modifier-row range.
    #[allow(clippy::too_many_arguments)]
    fn build_modifier_enum_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        enum_row: &EnumRowValue,
        object_index: usize,
        modifier_index: usize,
        param_slot: usize,
    ) -> f32 {
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let row = &enum_row.row;
        let label_text = enum_row
            .labels
            .get(row.value.round().clamp(0.0, (enum_row.labels.len().max(1) - 1) as f32) as usize)
            .copied()
            .unwrap_or("?");
        if row.driven {
            tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                inner_w - LABEL_W,
                ROW_H,
                &format!("{label_text} (driven)"),
                driven_label_style(),
            );
            return cy + ROW_H;
        }
        let base = MODIFIER_OFF_PARAM_BASE + param_slot as u64 * 3;
        let step_x = inner_x + inner_w - VALUE_W - STEP_W * 2.0;
        let minus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{2212}",
            modifier_row_key(object_index, modifier_index, base),
        );
        let value_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W,
            cy,
            VALUE_W,
            ROW_H,
            drag_value_style(),
            label_text,
            modifier_row_key(object_index, modifier_index, base + 1),
        );
        if let Some(name) = modifier_param_row_automation_name(param_slot) {
            tree.set_name(value_id, name);
        }
        let plus_id = tree.add_button_keyed(
            Some(self.content_parent),
            step_x + STEP_W + VALUE_W,
            cy,
            STEP_W,
            ROW_H,
            btn_style(),
            "\u{002B}",
            modifier_row_key(object_index, modifier_index, base + 2),
        );
        self.object_steppers.push((minus_id, row.clone(), -1.0));
        self.object_enum_cells.push((value_id, row.clone(), enum_row.labels.clone()));
        self.object_steppers.push((plus_id, row.clone(), 1.0));
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

    /// A "3 compact triplet" row (D4): label + X/Y/Z drag-value cells, no
    /// steppers (Position/Rotation/Scale/Color all use this shape). Driven
    /// axes render read-only with the same styling `build_numeric_row` uses.
    /// `mod_base_offset`/`object_label` are `None`/unused when this triplet
    /// isn't exposable — `build_color_row`'s internal R/G/B cells (per-
    /// channel exposure isn't part of UX-P3a's scope; the swatch itself is
    /// display-only, D4). When exposable, each axis is independently
    /// exposable (`transform_3d.pos_x`/`pos_y`/`pos_z` are three distinct
    /// inner params) — one small mod button sits right of each cell, not
    /// one shared button for the row.
    #[allow(clippy::too_many_arguments)]
    fn build_triplet_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        triplet: &(RowValue, RowValue, RowValue),
        index: usize,
        base_offset: u64,
        object_label: Option<&str>,
        mod_base_offset: Option<u64>,
    ) -> f32 {
        let reserve = mod_base_offset.is_some();
        let per_cell_reserve = if reserve { MOD_BTN_W + MOD_BTN_GAP } else { 0.0 };
        tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
        let cell_x = inner_x + LABEL_W;
        let cell_total_w = (inner_w - LABEL_W) / 3.0 - 2.0;
        let cell_w = (cell_total_w - per_cell_reserve).max(20.0);
        const AXIS: [&str; 3] = ["X", "Y", "Z"];
        for (i, row) in [&triplet.0, &triplet.1, &triplet.2].into_iter().enumerate() {
            // Each cell's slot is `cell_total_w` wide (value cell + its own
            // reserved mod-button strip, when exposable); `+2.0` is the same
            // inter-cell gap the un-reserved layout always used.
            let x = cell_x + i as f32 * (cell_total_w + 2.0);
            let mod_x = x + cell_w + MOD_BTN_GAP;
            let degrees = is_degrees_param(&row.addr.param_id);
            if row.driven {
                let text = if degrees {
                    format!("{:.1}\u{00b0}\u{2022}", row.value.to_degrees())
                } else {
                    format!("{:.2}\u{2022}", row.value)
                };
                tree.add_label(
                    Some(self.content_parent),
                    x,
                    cy,
                    cell_w,
                    ROW_H,
                    &text,
                    driven_label_style(),
                );
                if let (Some(obj), Some(mod_base)) = (object_label, mod_base_offset) {
                    let param_label = format!("{label} {}", AXIS[i]);
                    self.build_mod_button(
                        tree,
                        mod_x,
                        cy,
                        row,
                        obj,
                        &param_label,
                        degrees,
                        obj_key(index, mod_base + i as u64),
                        mod_button_automation_name(mod_base, i),
                    );
                }
                continue;
            }
            // D10: the committed degrees rows (`transform_3d.rot_*`) display
            // `%.1f°`; storage/commit still speak radians (`row.value`).
            let text = if degrees {
                format!("{:.1}\u{00b0}", row.value.to_degrees())
            } else {
                format!("{:.2}", row.value)
            };
            let cell_id = tree.add_button_keyed(
                Some(self.content_parent),
                x,
                cy,
                cell_w,
                ROW_H,
                drag_value_style(),
                &text,
                obj_key(index, base_offset + i as u64),
            );
            // Stable automation name (same fix as `build_numeric_row`'s
            // fixed rows) — `nth` picks which object's cell a flow means,
            // per the audio dock's own `name` + `nth` convention.
            if let Some(name) = triplet_cell_automation_name(base_offset, i) {
                tree.set_name(cell_id, name);
            }
            // UX-P2 (D3b): a thin accent hairline while THIS cell is the
            // one being scrubbed — the `MOD_TAB_INK_H`-scale idiom
            // (param_card.rs:47), a static 2px bar rather than that idiom's
            // sliding tween (there's nothing to slide between: one cell is
            // either being scrubbed or it isn't). No new style constant —
            // the fill reuses the slider rows' own accent
            // (`color::SLIDER_FILL_C32`), tying the two value shapes'
            // "active" language together (D7).
            if self.drag.is_active() && self.drag.payload().is_some_and(|d| d.addr == row.addr) {
                tree.add_panel(
                    Some(cell_id),
                    x,
                    cy + ROW_H - 2.0,
                    cell_w,
                    2.0,
                    UIStyle { bg_color: color::SLIDER_FILL_C32, ..UIStyle::default() },
                );
            }
            self.object_value_cells.push((cell_id, row.clone()));
            if let (Some(obj), Some(mod_base)) = (object_label, mod_base_offset) {
                let param_label = format!("{label} {}", AXIS[i]);
                self.build_mod_button(
                    tree,
                    mod_x,
                    cy,
                    row,
                    obj,
                    &param_label,
                    degrees,
                    obj_key(index, mod_base + i as u64),
                    mod_button_automation_name(mod_base, i),
                );
            }
        }
        cy + ROW_H
    }

    /// UX-P2 (D4 of SCENE_PANEL_UX_DESIGN.md): the Color row — a live
    /// square swatch (display-only, no picker: D4's rejected alternative)
    /// left of the SAME R/G/B `build_triplet_row` cells every other object
    /// uses. The swatch reads the identical `triplet` values the R/G/B
    /// cells render, so it updates on the exact same cadence — live during
    /// a scrub, with no new state or sync path.
    fn build_color_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        triplet: &(RowValue, RowValue, RowValue),
        index: usize,
        base_offset: u64,
    ) -> f32 {
        let to_u8 = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
        let swatch_color = Color32::new(to_u8(triplet.0.value), to_u8(triplet.1.value), to_u8(triplet.2.value), 255);
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
        self.build_triplet_row(
            tree,
            inner_x + SWATCH_W + ROW_GAP,
            inner_w - SWATCH_W - ROW_GAP,
            cy,
            "Color",
            triplet,
            index,
            base_offset,
            None,
            None,
        )
    }

    /// UX-P2 (D2 of SCENE_PANEL_UX_DESIGN.md): a bounded material scalar
    /// (Metallic/Roughness) as a real `BitmapSlider` row — shaped like
    /// param_card.rs's scalar rows (label left, fill bar, value right),
    /// replacing the old `[−] value [+]` stepper triplet
    /// (`build_object_numeric_row`). Drag lifecycle is the shared
    /// `SliderDragState` machine (`layer_header.rs`'s gain-slider
    /// precedent): `PointerDown`/`Drag`/`DragEnd` in `handle_event` drive
    /// `which`'s slider directly by track id, absolute-position (not
    /// delta), so one drag can sweep the full range — the performer
    /// gesture this design names.
    #[allow(clippy::too_many_arguments)]
    fn build_object_slider_row(
        &mut self,
        tree: &mut UITree,
        inner_x: f32,
        inner_w: f32,
        cy: f32,
        label: &str,
        row: &RowValue,
        layer_id: &LayerId,
        base_offset: u64,
        which: ObjSlider,
        index: usize,
        object_label: &str,
        mod_key_offset: u64,
    ) -> f32 {
        // UX-P3a: reserve the mod-button slot out of the row's width before
        // laying out the slider — same convention as the numeric/triplet
        // rows.
        let mod_x = inner_x + inner_w - MOD_BTN_W;
        let inner_w = inner_w - MOD_BTN_W - MOD_BTN_GAP;
        if row.driven {
            tree.add_label(Some(self.content_parent), inner_x, cy, LABEL_W, ROW_H, label, label_style());
            tree.add_label(
                Some(self.content_parent),
                inner_x + LABEL_W,
                cy,
                inner_w - LABEL_W,
                ROW_H,
                &format!("{:.2} (driven)", row.value),
                driven_label_style(),
            );
            self.build_mod_button(
                tree, mod_x, cy, row, object_label, label, false, obj_key(index, mod_key_offset),
                mod_button_automation_name(mod_key_offset, 0),
            );
            return cy + ROW_H;
        }
        let norm = crate::slider::BitmapSlider::value_to_normalized(row.value, row.min, row.max);
        let text = format!("{:.2}", row.value);
        // The reset action is structurally required by `BitmapSlider::build`
        // but this phase wires no right-click gesture for it (the panel has
        // never had a reset gesture on any row — D2 doesn't add one); it
        // writes the row's own current value back to itself, an inert no-op
        // unless a later phase registers the gesture.
        let inert_reset = PanelAction::SceneSetupParamChanged(
            layer_id.clone(),
            row.addr.scope_path.clone(),
            row.addr.node_doc_id,
            row.addr.param_id.clone(),
            row.value,
        );
        let built = crate::slider::BitmapSlider::build(
            tree,
            Some(self.content_parent),
            Rect::new(inner_x, cy, inner_w, ROW_H),
            Some(label),
            norm,
            &text,
            &crate::slider::SliderColors::default_slider(),
            color::FONT_LABEL,
            LABEL_W,
            norm,
            inert_reset,
        );
        if let Some(name) = object_slider_row_automation_name(base_offset) {
            tree.set_name(built.ids.value_text, name);
        }
        let (slider, slider_row) = match which {
            ObjSlider::Metallic => (&mut self.metallic_slider, &mut self.metallic_slider_row),
            ObjSlider::Roughness => (&mut self.roughness_slider, &mut self.roughness_slider_row),
        };
        slider.set_range(row.min, row.max, false);
        slider.set_ids(built.ids);
        *slider_row = Some(row.clone());
        self.build_mod_button(
            tree, mod_x, cy, row, object_label, label, false, obj_key(index, mod_key_offset),
            mod_button_automation_name(mod_key_offset, 0),
        );
        cy + ROW_H
    }

    /// Mouse-wheel scroll for the docked body.
    pub fn handle_scroll(&mut self, delta: f32) -> bool {
        self.scroll.apply_scroll_delta(-delta)
    }

    /// Whether a point lands inside the panel's own rect — for the app's
    /// drag-ownership dispatch (mirrors `AudioSetupPanel::point_in_panel`).
    pub fn point_in_panel(&self, pos: crate::node::Vec2) -> bool {
        self.open && self.panel_rect.contains(pos)
    }

    /// UX-P2 (D3a): whether `pos` is over a drag-armable value cell this
    /// frame — every cell in the SAME lookup set `PointerDown`'s
    /// delta-drag arm uses (fixed `row_ids` + object/light/camera value
    /// cells; the two slider tracks have their OWN cursor language, a
    /// resize handle doesn't apply to a track you click-to-jump on, so
    /// they're deliberately not included). The app's cursor-priority chain
    /// (`app.rs::update_cursor_for_position`) calls this to switch to
    /// `TimelineCursor::ResizeHorizontal` on hover — the visible half of
    /// D3a's affordance (the background-lighten half already ships via
    /// `drag_value_style`'s `hover_bg_color`).
    pub fn value_cell_at(&self, tree: &UITree, pos: crate::node::Vec2) -> bool {
        if !self.open {
            return false;
        }
        let hit = |id: NodeId| tree.get_bounds(id).contains(pos);
        self.row_ids.iter().filter_map(|ids| ids.value).any(hit)
            || self.object_value_cells.iter().any(|(id, _)| hit(*id))
            || self.light_value_cells.iter().any(|(id, _)| hit(*id))
            || self.camera_value_cells.iter().any(|(id, _)| hit(*id))
    }

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
                    } else if let Some((row_value, delta)) = stepper_hit_in(&self.object_steppers, *node_id)
                        .or_else(|| stepper_hit_in(&self.light_steppers, *node_id))
                        .or_else(|| stepper_hit_in(&self.camera_steppers, *node_id))
                    {
                        let new_value = (row_value.value + delta).clamp(row_value.min, row_value.max);
                        actions.push(PanelAction::SceneSetupParamChanged(
                            vm.layer_id.clone(),
                            row_value.addr.scope_path.clone(),
                            row_value.addr.node_doc_id,
                            row_value.addr.param_id.clone(),
                            new_value,
                        ));
                    } else if let Some((row, delta)) = self.stepper_hit(*node_id)
                        && let Some(row_value) = self.row_value_for(vm, row)
                    {
                        let new_value = (row_value.value + delta).clamp(row_value.min, row_value.max);
                        actions.push(PanelAction::SceneSetupParamChanged(
                            vm.layer_id.clone(),
                            row_value.addr.scope_path.clone(),
                            row_value.addr.node_doc_id,
                            row_value.addr.param_id.clone(),
                            new_value,
                        ));
                    } else if let Some((cell_id, row_value, labels)) = self
                        .object_enum_cells
                        .iter()
                        .chain(self.light_enum_cells.iter())
                        .find(|(id, _, _)| *id == *node_id)
                    {
                        // D9: 3+-label enum cells open the dropdown; 2-label
                        // cells stay a stepper (the `[-]/[+]` cycle above
                        // already covers them — this arm never fires for a
                        // 2-label row, so there's nothing to leave dead).
                        if labels.len() >= 3 && !row_value.driven {
                            actions.push(PanelAction::SceneSetupEnumClicked {
                                layer_id: vm.layer_id.clone(),
                                scope_path: row_value.addr.scope_path.clone(),
                                node_doc_id: row_value.addr.node_doc_id,
                                param_id: row_value.addr.param_id.clone(),
                                labels: labels.clone(),
                                current_index: row_value.value.round().max(0.0) as u32,
                                cell_node_id: *cell_id,
                            });
                        }
                    } else if let Some((_, ctx)) = self.mod_button_ids.iter().find(|(id, _)| *id == *node_id) {
                        // UX-P3a: the panel always emits — a re-click on an
                        // already-exposed row is a harmless duplicate the
                        // app dispatch handler no-ops on (see
                        // `PanelAction::SceneSetupExposeParam`'s doc).
                        actions.push(PanelAction::SceneSetupExposeParam {
                            layer_id: vm.layer_id.clone(),
                            scope_path: ctx.addr.scope_path.clone(),
                            node_doc_id: ctx.addr.node_doc_id,
                            param_id: ctx.addr.param_id.clone(),
                            object_label: ctx.object_label.clone(),
                            param_label: ctx.param_label.clone(),
                            min: ctx.min,
                            max: ctx.max,
                            default_value: ctx.value,
                            is_angle: ctx.is_angle,
                        });
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
            // EXACT lookup PointerDown uses below (row_ids fixed rows +
            // object/light/camera_value_cells) — the same set arms both
            // gestures by construction, which is the drag/type-in
            // registration parity `dock_numeric_cells_register_full_contract`
            // checks.
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
                        .value_label_row_at(*node_id)
                        .and_then(|row| self.row_value_for(vm, row))
                        .or_else(|| {
                            self.object_value_cells
                                .iter()
                                .chain(self.light_value_cells.iter())
                                .chain(self.camera_value_cells.iter())
                                .find(|(id, _)| *id == *node_id)
                                .map(|(_, rv)| rv.clone())
                        })
                        .or_else(|| {
                            // UX-P2 (D2): the slider's own value-box node —
                            // `BitmapSlider`'s contract defines the SAME
                            // ValueCell+DoubleClick→EditValue intent
                            // (slider.rs `intent_for`), so double-click-to-
                            // type keeps working on Metallic/Roughness now
                            // that they're sliders, not value cells.
                            self.metallic_slider
                                .ids()
                                .filter(|ids| ids.value_text == *node_id)
                                .and(self.metallic_slider_row.clone())
                                .or_else(|| {
                                    self.roughness_slider
                                        .ids()
                                        .filter(|ids| ids.value_text == *node_id)
                                        .and(self.roughness_slider_row.clone())
                                })
                        });
                    if let Some(row_value) = row_value
                        && !row_value.driven
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
            UIEvent::PointerDown { node_id, pos, modifiers } => {
                if let SceneSetupState::Live(vm) = &self.state {
                    // UX-P2 (D2): Metallic/Roughness sliders — track-hit,
                    // absolute-position (not delta), so one drag sweeps the
                    // row's full range. Checked before the delta-drag value
                    // cells below since a slider's track is a distinct node.
                    if let Some(new_value) = self.metallic_slider.try_start_drag(*node_id, pos.x)
                        && let Some(row) = &self.metallic_slider_row
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        return (
                            true,
                            vec![PanelAction::SceneSetupParamChanged(
                                vm.layer_id.clone(),
                                row.addr.scope_path.clone(),
                                row.addr.node_doc_id,
                                row.addr.param_id.clone(),
                                new_value,
                            )],
                        );
                    }
                    if let Some(new_value) = self.roughness_slider.try_start_drag(*node_id, pos.x)
                        && let Some(row) = &self.roughness_slider_row
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        return (
                            true,
                            vec![PanelAction::SceneSetupParamChanged(
                                vm.layer_id.clone(),
                                row.addr.scope_path.clone(),
                                row.addr.node_doc_id,
                                row.addr.param_id.clone(),
                                new_value,
                            )],
                        );
                    }
                    if let Some(row) = self.value_label_row_at(*node_id)
                        && let Some(row_value) = self.row_value_for(vm, row)
                        && !row_value.driven
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        self.drag.start(
                            ValueDrag {
                                addr: row_value.addr.clone(),
                                start_x: pos.x,
                                start_value: row_value.value,
                                min: row_value.min,
                                max: row_value.max,
                                fine: modifiers.shift,
                            },
                            *pos,
                        );
                        return (true, Vec::new());
                    }
                    if let Some((_, row_value)) = self
                        .object_value_cells
                        .iter()
                        .chain(self.light_value_cells.iter())
                        .chain(self.camera_value_cells.iter())
                        .find(|(id, _)| *id == *node_id)
                        && !row_value.driven
                    {
                        self.drag_layer_id = Some(vm.layer_id.clone());
                        self.drag.start(
                            ValueDrag {
                                addr: row_value.addr.clone(),
                                start_x: pos.x,
                                start_value: row_value.value,
                                min: row_value.min,
                                max: row_value.max,
                                fine: modifiers.shift,
                            },
                            *pos,
                        );
                        return (true, Vec::new());
                    }
                }
                (self.owns_node(*node_id) || self.point_in_panel(*pos), Vec::new())
            }
            UIEvent::DragBegin { .. } => {
                (self.drag.is_active() || self.metallic_slider.is_dragging() || self.roughness_slider.is_dragging(), Vec::new())
            }
            UIEvent::Drag { pos, .. } => {
                // UX-P2 (D2): continue an active slider drag first (distinct
                // machinery from the delta-drag `self.drag` below — see
                // `slider_drag_value`'s doc for why there's no local tree
                // update here).
                if let Some(layer_id) = self.drag_layer_id.clone() {
                    if let Some(new_value) = slider_drag_value(&self.metallic_slider, pos.x)
                        && let Some(row) = &self.metallic_slider_row
                    {
                        return (
                            true,
                            vec![PanelAction::SceneSetupParamChanged(
                                layer_id,
                                row.addr.scope_path.clone(),
                                row.addr.node_doc_id,
                                row.addr.param_id.clone(),
                                new_value,
                            )],
                        );
                    }
                    if let Some(new_value) = slider_drag_value(&self.roughness_slider, pos.x)
                        && let Some(row) = &self.roughness_slider_row
                    {
                        return (
                            true,
                            vec![PanelAction::SceneSetupParamChanged(
                                layer_id,
                                row.addr.scope_path.clone(),
                                row.addr.node_doc_id,
                                row.addr.param_id.clone(),
                                new_value,
                            )],
                        );
                    }
                }
                match (self.drag.payload().cloned(), self.drag_layer_id.clone()) {
                    (Some(drag), Some(layer_id)) => {
                        // D8: Shift held at drag-start ("fine") multiplies the
                        // applied per-pixel delta by 0.1. D10: the committed
                        // degrees-display rows scrub in degrees (0.5°/px, fine
                        // 0.05°/px) converted to the radians the graph stores —
                        // the ONLY place that conversion happens; everywhere
                        // else keeps the existing 1 px = 0.01 units rate (the
                        // audio dock's 0.1 dB/px order of magnitude, scaled for
                        // these params' typical [0, ~2] ranges).
                        let dx = pos.x - drag.start_x;
                        let delta = if is_degrees_param(&drag.addr.param_id) {
                            let deg_per_px = if drag.fine { 0.05 } else { 0.5 };
                            (dx * deg_per_px).to_radians()
                        } else {
                            let unit_per_px = if drag.fine { 0.001 } else { 0.01 };
                            dx * unit_per_px
                        };
                        let new_value = (drag.start_value + delta).clamp(drag.min, drag.max);
                        (
                            true,
                            vec![PanelAction::SceneSetupParamChanged(
                                layer_id,
                                drag.addr.scope_path.clone(),
                                drag.addr.node_doc_id,
                                drag.addr.param_id.clone(),
                                new_value,
                            )],
                        )
                    }
                    _ => (false, Vec::new()),
                }
            }
            UIEvent::DragEnd { .. } | UIEvent::PointerUp { .. } => {
                self.drag.release();
                self.metallic_slider.end_drag();
                self.roughness_slider.end_drag();
                self.drag_layer_id = None;
                (false, Vec::new())
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

    fn stepper_hit(&self, node_id: NodeId) -> Option<(u64, f32)> {
        for (row, ids) in self.row_ids.iter().enumerate() {
            if ids.minus == Some(node_id) {
                return Some((row as u64, -0.05));
            }
            if ids.plus == Some(node_id) {
                return Some((row as u64, 0.05));
            }
        }
        None
    }

    fn value_label_row_at(&self, node_id: NodeId) -> Option<u64> {
        self.row_ids
            .iter()
            .position(|ids| ids.value == Some(node_id))
            .map(|row| row as u64)
    }

    fn row_value_for(&self, vm: &SceneSetupVm, row: u64) -> Option<RowValue> {
        match row {
            ROW_ENV_INTENSITY => match &vm.environment {
                EnvironmentRowVm::Importer { intensity, .. } | EnvironmentRowVm::Bare { intensity, .. } => {
                    Some(intensity.clone())
                }
                _ => None,
            },
            ROW_ENV_FILL => match &vm.environment {
                EnvironmentRowVm::Importer { fill, .. } | EnvironmentRowVm::Bare { fill, .. } => Some(fill.clone()),
                _ => None,
            },
            ROW_FOG_DENSITY => match &vm.atmosphere {
                AtmosphereRowVm::Wired { density, .. } => Some(density.clone()),
                AtmosphereRowVm::None => None,
            },
            ROW_FOG_HEIGHT_FALLOFF => match &vm.atmosphere {
                AtmosphereRowVm::Wired { height_falloff, .. } => Some(height_falloff.clone()),
                AtmosphereRowVm::None => None,
            },
            _ => None,
        }
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

/// Generic stepper hit test over a variable-length list built this frame —
/// the shared shape `object_stepper_hit` used before Lights/Camera needed
/// the identical lookup (Objects/Lights are variable-length; Camera is a
/// fixed single row set, but its `+`/`-` buttons are captured the same way).
fn stepper_hit_in(steppers: &[(NodeId, RowValue, f32)], node_id: NodeId) -> Option<(RowValue, f32)> {
    steppers.iter().find(|(id, _, _)| *id == node_id).map(|(_, row, delta)| (row.clone(), *delta))
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

/// UX-P3a mod button (D8/D9): the SAME `state_button_skinned` skin every
/// T/∿/A drawer-tab button uses (`param_slider_shared::de_btn_style`), lit
/// with the SAME accent `param_card.rs`'s Driver tab uses
/// (`DRIVER_ACTIVE_C32`) — one button, not the full strip (no drawer opens
/// in THIS panel this phase — D9's row+drawer reuse is UX-P3b), so the
/// strip degenerates to a single "make this modulatable" glyph that lights
/// the moment the param is exposed on the card. A driven row passes
/// `active: false` regardless of `exposed` — its slot is reserved (EyeSlot's
/// Live/Dimmed convention: present, never absent) but `build_mod_button`
/// never pushes it into `mod_button_ids`, so it isn't a click target even
/// though it renders the same "off" skin an unexposed-but-live row does.
fn mod_btn_style(active: bool) -> UIStyle {
    crate::panels::param_slider_shared::de_btn_style(active, color::DRIVER_ACTIVE_C32)
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

    fn triplet(node_doc_id: u32, x: f32, y: f32, z: f32, min: f32, max: f32) -> (RowValue, RowValue, RowValue) {
        (
            RowValue { addr: RowAddr::root(node_doc_id, "x"), value: x, min, max, driven: false, exposed: false },
            RowValue { addr: RowAddr::root(node_doc_id, "y"), value: y, min, max, driven: false, exposed: false },
            RowValue { addr: RowAddr::root(node_doc_id, "z"), value: z, min, max, driven: false, exposed: false },
        )
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
            environment: EnvironmentRowVm::Bare { intensity, fill: env_row(0.0) },
            atmosphere: AtmosphereRowVm::None,
            objects: Vec::new(),
            lights: Vec::new(),
            camera: CameraRowVm::None,
        })));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(panel.row_ids[ROW_ENV_INTENSITY as usize].minus.is_none());
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
                        pos: triplet(50, 1.0, 2.0, 3.0, -100.0, 100.0),
                        rot: triplet(50, 0.0, 0.0, 0.0, -6.28, 6.28),
                        scale: triplet(50, 1.0, 1.0, 1.0, 0.01, 10.0),
                    })),
                    material: ObjectMaterialVm::Pbr {
                        color: triplet(51, 0.8, 0.8, 0.82, 0.0, 1.0),
                        metallic: RowValue { addr: RowAddr::root(51, "metallic"), value: 0.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                        roughness: RowValue { addr: RowAddr::root(51, "roughness"), value: 0.5, min: 0.01, max: 1.0, driven: false, exposed: false },
                    },
                    modifiers: vec![ModifierKnownRow {
                        index: 0,
                        node_doc_id: 70,
                        display_name: "Bend".to_string(),
                        params: vec![
                            ModifierParamRowVm::Axis {
                                label: "Axis",
                                row: EnumRowValue {
                                    row: RowValue { addr: RowAddr { scope_path: vec![42], node_doc_id: 70, param_id: "axis".to_string() }, value: 1.0, min: 0.0, max: 2.0, driven: false, exposed: false },
                                    labels: vec!["X", "Y", "Z"],
                                },
                            },
                            ModifierParamRowVm::Numeric {
                                label: "Angle",
                                row: RowValue { addr: RowAddr { scope_path: vec![42], node_doc_id: 70, param_id: "angle".to_string() }, value: 0.5, min: -6.28, max: 6.28, driven: false, exposed: false },
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
                    mode: EnumRowValue {
                        row: RowValue { addr: RowAddr::root(60, "mode"), value: 0.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                        labels: vec!["Sun", "Point"],
                    },
                    color: triplet(60, 1.0, 1.0, 1.0, 0.0, 1.0),
                    intensity: RowValue { addr: RowAddr::root(60, "intensity"), value: 2.5, min: 0.0, max: 10.0, driven: false, exposed: false },
                    pos: triplet(60, 5.0, 2.0, 3.0, -100.0, 100.0),
                    aim: triplet(60, 0.0, 0.0, 0.0, -100.0, 100.0),
                    cast_shadows: EnumRowValue {
                        row: RowValue { addr: RowAddr::root(60, "cast_shadows"), value: 1.0, min: 0.0, max: 1.0, driven: false, exposed: false },
                        labels: vec!["Off", "On"],
                    },
                    shadow_softness: EnumRowValue {
                        row: RowValue { addr: RowAddr::root(60, "shadow_softness"), value: 3.0, min: 0.0, max: 3.0, driven: false, exposed: false },
                        labels: vec!["Hard", "Soft", "VerySoft", "Contact"],
                    },
                    light_size: RowValue { addr: RowAddr::root(60, "light_size"), value: 4.0, min: 0.0, max: 20.0, driven: false, exposed: false },
                })),
                LightRowVm::Custom { index: 1 },
            ],
            camera: CameraRowVm::Orbit(Box::new(OrbitCameraRowVm {
                orbit: RowValue { addr: RowAddr::root(70, "orbit"), value: 0.7, min: -6.28, max: 6.28, driven: false, exposed: false },
                tilt: RowValue { addr: RowAddr::root(70, "tilt"), value: 0.3, min: -6.28, max: 6.28, driven: false, exposed: false },
                distance: RowValue { addr: RowAddr::root(70, "distance"), value: 4.0, min: 0.01, max: 100.0, driven: false, exposed: false },
                fov_y: RowValue { addr: RowAddr::root(70, "fov_y"), value: 0.9, min: 0.05, max: 2.5, driven: false, exposed: false },
                lens: Some(LensRowVm {
                    focus_distance: RowValue { addr: RowAddr::root(71, "focus_distance"), value: 0.0, min: 0.0, max: 1000.0, driven: false, exposed: false },
                    f_stop: RowValue { addr: RowAddr::root(71, "f_stop"), value: 1000.0, min: 0.5, max: 1000.0, driven: false, exposed: false },
                    shutter_angle: RowValue { addr: RowAddr::root(71, "shutter_angle"), value: 0.0, min: 0.0, max: 360.0, driven: false, exposed: false },
                    exposure_ev: RowValue { addr: RowAddr::root(71, "exposure_ev"), value: 0.0, min: -8.0, max: 8.0, driven: false, exposed: false },
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
        // IS the fold): 3 transform triplets (9 cells) + 1 color triplet (3
        // cells) = 12 drag cells; the one Bend modifier's Angle param
        // (Numeric) adds 1 more (its Axis param is an Enum row — no
        // drag-value cell). UX-P2 (D2): metallic/roughness moved OFF this
        // vector onto the two `BitmapSlider` rows (`metallic_slider`/
        // `roughness_slider`) — asserted separately below.
        assert_eq!(
            panel.object_value_cells.len(),
            13,
            "9 transform + 3 color + 1 modifier numeric param value cell"
        );
        assert!(panel.metallic_slider_row.is_some(), "Metallic renders as a slider row");
        assert!(panel.roughness_slider_row.is_some(), "Roughness renders as a slider row");
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

    /// UX-P2 (D2): the Roughness slider's track is drag-armable (a click
    /// anywhere on the track jumps straight to that value, then Drag
    /// continues absolute-position — the "sweep full-range in one drag"
    /// performer gesture the phase brief names), and its separate
    /// value-text box still opens the type-in box on double-click (D8
    /// parity, `dock_numeric_cells_register_full_contract`'s sibling for
    /// the two-node slider shape that test's single-node loop can't cover).
    #[test]
    fn roughness_slider_sweeps_full_range_and_value_box_opens_typein() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let ids = panel.roughness_slider.ids().expect("Roughness renders a slider").clone();
        let track = ids.track_rect;

        // PointerDown at the track's left edge — jumps to (near) min.
        let (consumed, actions) = panel.handle_event(&UIEvent::PointerDown {
            node_id: ids.track,
            pos: Vec2::new(track.x, 0.0),
            modifiers: crate::input::Modifiers::default(),
        });
        assert!(consumed, "the slider track must be drag-armable");
        let PanelAction::SceneSetupParamChanged(_, _, _, param, low_value) = &actions[0] else {
            panic!("expected SceneSetupParamChanged, got {actions:?}");
        };
        assert_eq!(param, "roughness");
        assert!(*low_value < 0.05, "PointerDown at the track's left edge lands near min, got {low_value}");

        // Drag to the track's right edge — one gesture sweeps to (near) max,
        // continuing off the SAME `try_start_drag` arm via `slider_drag_value`.
        let (drag_consumed, drag_actions) = panel.handle_event(&UIEvent::Drag {
            node_id: Some(ids.track),
            pos: Vec2::new(track.x + track.width, 0.0),
            delta: Vec2::ZERO,
        });
        assert!(drag_consumed);
        let PanelAction::SceneSetupParamChanged(_, _, _, _, high_value) = &drag_actions[0] else {
            panic!("expected SceneSetupParamChanged, got {drag_actions:?}");
        };
        assert!(*high_value > 0.95, "dragging to the track's right edge lands near max, got {high_value}");

        panel.handle_event(&UIEvent::PointerUp { node_id: Some(ids.track), pos: Vec2::new(track.x + track.width, 0.0) });
        assert!(!panel.roughness_slider.is_dragging(), "PointerUp ends the drag");

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

    /// UX-P3a (SCENE_PANEL_UX_DESIGN.md D8, sizing amendment): clicking an
    /// unexposed row's mod button emits `SceneSetupExposeParam` named
    /// `<ObjectName> · <ParamLabel>` — proven on the Roughness slider row
    /// (`build_object_slider_row`), the flagship performer-gesture surface
    /// D8's own text names. A second click on the SAME (now still-unexposed,
    /// since this panel never mutates `RowValue` itself) button emits the
    /// SAME action again — the panel's one-way "always emit, app no-ops"
    /// contract (see the action's own doc comment).
    #[test]
    fn mod_button_click_emits_expose_param_named_object_and_param() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(!panel.mod_button_ids.is_empty(), "at least one exposable row must be built");

        let (roughness_id, ctx) = panel
            .mod_button_ids
            .iter()
            .find(|(_, ctx)| ctx.param_label == "Roughness")
            .expect("Metallic/Roughness sliders are mod-button rows");
        assert_eq!(ctx.object_label, "Azalea");
        assert_eq!(ctx.addr.param_id, "roughness");

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: *roughness_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::SceneSetupExposeParam { layer_id, param_id, object_label, param_label, .. } => {
                assert_eq!(*layer_id, LayerId::new("layer-1"));
                assert_eq!(param_id, "roughness");
                assert_eq!(object_label, "Azalea");
                assert_eq!(param_label, "Roughness");
            }
            other => panic!("expected SceneSetupExposeParam, got {other:?}"),
        }
    }

    /// A driven triplet cell (e.g. a wire-shadowed transform axis) still
    /// draws its mod-button slot (EyeSlot's reserved-but-dimmed convention)
    /// but does NOT register a click target — `mod_button_ids` has no entry
    /// for it, so a click there is a no-op, never an exposure of a param
    /// that's already receiving a wire from somewhere else.
    #[test]
    fn driven_row_mod_button_is_reserved_but_not_clickable() {
        let mut vm = azalea_shaped_vm();
        let SceneSetupVm { objects, .. } = &mut vm;
        let ObjectRowVm::Known(obj) = &mut objects[0] else { panic!("Azalea must be Known") };
        obj.transform.as_mut().unwrap().pos.0.driven = true;
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(
            !panel.mod_button_ids.iter().any(|(_, ctx)| ctx.param_label == "Position X"),
            "a driven row's mod button must not be a click target"
        );
    }

    /// UX-P3b-i: the same mod-button parity as
    /// `mod_button_click_emits_expose_param_named_object_and_param`, proven
    /// on a LIGHT row (`build_light_numeric_row`'s Intensity field) — the
    /// family this phase adds. `Sun` is the azalea fixture's own light name
    /// (`azalea_shaped_vm`'s `LightKnownRow.name`).
    #[test]
    fn light_mod_button_click_emits_expose_param_named_light_and_param() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let (intensity_id, ctx) = panel
            .mod_button_ids
            .iter()
            .find(|(_, ctx)| ctx.object_label == "Sun" && ctx.param_label == "Intensity")
            .expect("the light's Intensity row must be a mod-button row");
        assert_eq!(ctx.addr.param_id, "intensity");

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: *intensity_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        match &actions[..] {
            [PanelAction::SceneSetupExposeParam { object_label, param_label, param_id, .. }] => {
                assert_eq!(object_label, "Sun");
                assert_eq!(param_label, "Intensity");
                assert_eq!(param_id, "intensity");
            }
            other => panic!("expected one SceneSetupExposeParam, got {other:?}"),
        }
    }

    /// UX-P3b-i: mod-button parity on a CAMERA row (`build_camera_numeric_row`'s
    /// Orbit field, the azalea fixture's `OrbitCameraRowVm`) — `object_label`
    /// is the fixed `"Camera"` string (no per-instance camera name exists).
    #[test]
    fn camera_mod_button_click_emits_expose_param_named_camera_and_param() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let (orbit_id, ctx) = panel
            .mod_button_ids
            .iter()
            .find(|(_, ctx)| ctx.object_label == "Camera" && ctx.param_label == "Orbit")
            .expect("the camera's Orbit row must be a mod-button row");
        assert_eq!(ctx.addr.param_id, "orbit");

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: *orbit_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        match &actions[..] {
            [PanelAction::SceneSetupExposeParam { object_label, param_label, .. }] => {
                assert_eq!(object_label, "Camera");
                assert_eq!(param_label, "Orbit");
            }
            other => panic!("expected one SceneSetupExposeParam, got {other:?}"),
        }
    }

    /// UX-P3b-i: mod-button parity on a MODIFIER param row
    /// (`build_modifier_numeric_row`'s "Angle" slot on the azalea fixture's
    /// Bend modifier) — `object_label` is the OWNING OBJECT's name ("Azalea"),
    /// and `param_label` is disambiguated by the modifier's own display name
    /// ("Bend Angle"), not a bare "Angle" that would collide across modifiers.
    #[test]
    fn modifier_mod_button_click_emits_expose_param_named_object_and_modifier_param() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        // azalea_shaped_vm's default selection already targets the Azalea object.
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let (angle_id, ctx) = panel
            .mod_button_ids
            .iter()
            .find(|(_, ctx)| ctx.object_label == "Azalea" && ctx.param_label == "Bend Angle")
            .expect("the Bend modifier's Angle row must be a mod-button row");
        assert_eq!(ctx.addr.param_id, "angle");

        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: *angle_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        match &actions[..] {
            [PanelAction::SceneSetupExposeParam { object_label, param_label, param_id, .. }] => {
                assert_eq!(object_label, "Azalea");
                assert_eq!(param_label, "Bend Angle");
                assert_eq!(param_id, "angle");
            }
            other => panic!("expected one SceneSetupExposeParam, got {other:?}"),
        }
        // The modifier's Axis row (a structural switch) must stay unexposable
        // — same reasoning as Light's Mode row.
        assert!(
            !panel.mod_button_ids.iter().any(|(_, ctx)| ctx.param_label.ends_with("Axis")),
            "a modifier's Axis row must not be a mod-button click target"
        );
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

        assert_no_dupes_and_fits_stride(
            "OBJECT",
            &[
                OBJ_OFF_NAME,
                OBJ_OFF_POS_X, OBJ_OFF_POS_X + 1, OBJ_OFF_POS_X + 2,
                OBJ_OFF_ROT_X, OBJ_OFF_ROT_X + 1, OBJ_OFF_ROT_X + 2,
                OBJ_OFF_SCALE_X, OBJ_OFF_SCALE_X + 1, OBJ_OFF_SCALE_X + 2,
                OBJ_OFF_COLOR_R, OBJ_OFF_COLOR_R + 1, OBJ_OFF_COLOR_R + 2,
                OBJ_OFF_METALLIC,
                OBJ_OFF_ROUGHNESS,
                OBJ_OFF_REMOVE, OBJ_OFF_REMOVE + 1,
                OBJ_OFF_POS_X_MOD, OBJ_OFF_POS_X_MOD + 1, OBJ_OFF_POS_X_MOD + 2,
                OBJ_OFF_ROT_X_MOD, OBJ_OFF_ROT_X_MOD + 1, OBJ_OFF_ROT_X_MOD + 2,
                OBJ_OFF_SCALE_X_MOD, OBJ_OFF_SCALE_X_MOD + 1, OBJ_OFF_SCALE_X_MOD + 2,
                OBJ_OFF_METALLIC_MOD,
                OBJ_OFF_ROUGHNESS_MOD,
            ],
            Some(OBJ_KEY_STRIDE),
        );

        assert_no_dupes_and_fits_stride(
            "LIGHT",
            &[
                LIGHT_OFF_MODE_MINUS, LIGHT_OFF_MODE_MINUS + 1, LIGHT_OFF_MODE_MINUS + 2,
                LIGHT_OFF_COLOR_R, LIGHT_OFF_COLOR_R + 1, LIGHT_OFF_COLOR_R + 2,
                LIGHT_OFF_INTENSITY_MINUS, LIGHT_OFF_INTENSITY_MINUS + 1, LIGHT_OFF_INTENSITY_MINUS + 2,
                LIGHT_OFF_POS_X, LIGHT_OFF_POS_X + 1, LIGHT_OFF_POS_X + 2,
                LIGHT_OFF_AIM_X, LIGHT_OFF_AIM_X + 1, LIGHT_OFF_AIM_X + 2,
                LIGHT_OFF_CAST_SHADOWS_MINUS, LIGHT_OFF_CAST_SHADOWS_MINUS + 1, LIGHT_OFF_CAST_SHADOWS_MINUS + 2,
                LIGHT_OFF_SHADOW_SOFTNESS_MINUS, LIGHT_OFF_SHADOW_SOFTNESS_MINUS + 1, LIGHT_OFF_SHADOW_SOFTNESS_MINUS + 2,
                LIGHT_OFF_LIGHT_SIZE_MINUS, LIGHT_OFF_LIGHT_SIZE_MINUS + 1, LIGHT_OFF_LIGHT_SIZE_MINUS + 2,
                LIGHT_OFF_REMOVE,
                LIGHT_OFF_NAME,
                LIGHT_OFF_INTENSITY_MOD,
                LIGHT_OFF_POS_X_MOD, LIGHT_OFF_POS_X_MOD + 1, LIGHT_OFF_POS_X_MOD + 2,
                LIGHT_OFF_AIM_X_MOD, LIGHT_OFF_AIM_X_MOD + 1, LIGHT_OFF_AIM_X_MOD + 2,
                LIGHT_OFF_CAST_SHADOWS_MOD,
                LIGHT_OFF_SHADOW_SOFTNESS_MOD,
                LIGHT_OFF_LIGHT_SIZE_MOD,
            ],
            Some(LIGHT_KEY_STRIDE),
        );

        // Camera has no per-index stride (exactly one row set per scene) —
        // just pairwise-distinct offsets, plus a headroom check against the
        // next section's base (`MODIFIER_KEY_BASE`, 2_000 above `CAMERA_KEY_BASE`).
        let camera_offsets = [
            CAMERA_OFF_ORBIT_MINUS, CAMERA_OFF_ORBIT_MINUS + 1, CAMERA_OFF_ORBIT_MINUS + 2,
            CAMERA_OFF_TILT_MINUS, CAMERA_OFF_TILT_MINUS + 1, CAMERA_OFF_TILT_MINUS + 2,
            CAMERA_OFF_DISTANCE_MINUS, CAMERA_OFF_DISTANCE_MINUS + 1, CAMERA_OFF_DISTANCE_MINUS + 2,
            CAMERA_OFF_FOV_MINUS, CAMERA_OFF_FOV_MINUS + 1, CAMERA_OFF_FOV_MINUS + 2,
            CAMERA_OFF_POS_X, CAMERA_OFF_POS_X + 1, CAMERA_OFF_POS_X + 2,
            CAMERA_OFF_YAW_MINUS, CAMERA_OFF_YAW_MINUS + 1, CAMERA_OFF_YAW_MINUS + 2,
            CAMERA_OFF_PITCH_MINUS, CAMERA_OFF_PITCH_MINUS + 1, CAMERA_OFF_PITCH_MINUS + 2,
            CAMERA_OFF_ROLL_MINUS, CAMERA_OFF_ROLL_MINUS + 1, CAMERA_OFF_ROLL_MINUS + 2,
            CAMERA_OFF_TARGET_X, CAMERA_OFF_TARGET_X + 1, CAMERA_OFF_TARGET_X + 2,
            CAMERA_OFF_LENS_FOCUS_MINUS, CAMERA_OFF_LENS_FOCUS_MINUS + 1, CAMERA_OFF_LENS_FOCUS_MINUS + 2,
            CAMERA_OFF_LENS_FSTOP_MINUS, CAMERA_OFF_LENS_FSTOP_MINUS + 1, CAMERA_OFF_LENS_FSTOP_MINUS + 2,
            CAMERA_OFF_LENS_SHUTTER_MINUS, CAMERA_OFF_LENS_SHUTTER_MINUS + 1, CAMERA_OFF_LENS_SHUTTER_MINUS + 2,
            CAMERA_OFF_LENS_EXPOSURE_MINUS, CAMERA_OFF_LENS_EXPOSURE_MINUS + 1, CAMERA_OFF_LENS_EXPOSURE_MINUS + 2,
            CAMERA_OFF_ORBIT_MOD,
            CAMERA_OFF_TILT_MOD,
            CAMERA_OFF_DISTANCE_MOD,
            CAMERA_OFF_FOV_MOD,
            CAMERA_OFF_POS_X_MOD, CAMERA_OFF_POS_X_MOD + 1, CAMERA_OFF_POS_X_MOD + 2,
            CAMERA_OFF_YAW_MOD,
            CAMERA_OFF_PITCH_MOD,
            CAMERA_OFF_ROLL_MOD,
            CAMERA_OFF_TARGET_X_MOD, CAMERA_OFF_TARGET_X_MOD + 1, CAMERA_OFF_TARGET_X_MOD + 2,
            CAMERA_OFF_LENS_FOCUS_MOD,
            CAMERA_OFF_LENS_FSTOP_MOD,
            CAMERA_OFF_LENS_SHUTTER_MOD,
            CAMERA_OFF_LENS_EXPOSURE_MOD,
        ];
        assert_no_dupes_and_fits_stride("CAMERA", &camera_offsets, None);
        assert!(
            camera_offsets.iter().all(|&o| CAMERA_KEY_BASE + o < MODIFIER_KEY_BASE),
            "a camera offset reaches into MODIFIER_KEY_BASE's range"
        );

        // Modifier: per-slot offsets (up to 4 param slots) must fit inside
        // MODIFIER_ROW_STRIDE, same per-index-range contract as OBJECT/LIGHT.
        for slot in 0..4u64 {
            let base = MODIFIER_OFF_PARAM_BASE + slot * 3;
            assert_no_dupes_and_fits_stride(
                "MODIFIER (per-row)",
                &[
                    MODIFIER_OFF_UP,
                    MODIFIER_OFF_DOWN,
                    MODIFIER_OFF_REMOVE,
                    base, base + 1, base + 2,
                    MODIFIER_OFF_PARAM_MOD_BASE + slot,
                ],
                Some(MODIFIER_ROW_STRIDE),
            );
        }
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
        // Default selection = Azalea: object body cells present, no
        // environment/fog "add" affordances (azalea fixture's environment
        // is None — but World isn't selected, so neither button builds).
        assert!(!panel.object_value_cells.is_empty(), "Azalea's body renders by default");
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
        assert!(panel.object_value_cells.is_empty(), "World selected — no object body renders");
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
        // The Known light's body (always rendered, no fold state left):
        // mode(1) + color triplet(3) + intensity(1) + pos triplet(3) + aim
        // triplet(3) + light_size(1) = value cells (cast_shadows/
        // shadow_softness are enum steppers, not drag-armable value cells).
        assert_eq!(panel.light_value_cells.len(), 11, "color+pos+aim triplets (9) + intensity + light_size");
        assert!(panel.add_light_id.is_some());
    }

    #[test]
    fn light_cast_shadows_and_shadow_softness_steppers_use_delta_one() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let enum_steppers: Vec<_> = panel.light_steppers.iter().filter(|(_, _, delta)| delta.abs() == 1.0).collect();
        // cast_shadows (minus/plus) + shadow_softness (minus/plus) + mode
        // (minus/plus) = 6 enum-stepper buttons.
        assert_eq!(enum_steppers.len(), 6, "mode/cast_shadows/shadow_softness steppers all use delta 1.0");
    }

    #[test]
    fn light_size_row_always_renders_even_when_softness_isnt_contact() {
        // D4: light_size is a parameter DEPENDENCY, not conditional UI — the
        // stepper must exist regardless of the current shadow_softness value.
        let mut vm = azalea_shaped_vm();
        if let LightRowVm::Known(row) = &mut vm.lights[0] {
            row.shadow_softness.row.value = 0.0; // Hard, not Contact
        }
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        assert!(
            panel.light_steppers.iter().any(|(_, row, _)| row.addr.param_id == "light_size"),
            "light_size stepper exists regardless of shadow_softness mode"
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
    fn dragging_light_intensity_value_cell_starts_a_drag_with_the_right_address() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (intensity_id, intensity_row) = panel
            .light_value_cells
            .iter()
            .find(|(_, row)| row.addr.param_id == "intensity")
            .cloned()
            .expect("intensity value cell built for the Known light row");
        assert_eq!(intensity_row.addr, RowAddr::root(60, "intensity"));

        let (consumed, actions) = panel.handle_event(&UIEvent::PointerDown {
            node_id: intensity_id,
            pos: crate::node::Vec2::new(0.0, 0.0),
            modifiers: Modifiers::default(),
        });
        assert!(consumed);
        assert!(actions.is_empty(), "PointerDown arms the drag, doesn't dispatch yet");

        let (consumed, actions) = panel.handle_event(&UIEvent::Drag {
            node_id: Some(intensity_id),
            pos: crate::node::Vec2::new(50.0, 0.0),
            delta: crate::node::Vec2::new(50.0, 0.0),
        });
        assert!(consumed);
        assert_eq!(actions.len(), 1);
        match &actions[0] {
            PanelAction::SceneSetupParamChanged(layer_id, scope, node_doc_id, param_id, value) => {
                assert_eq!(*layer_id, LayerId::new("layer-1"));
                assert!(scope.is_empty(), "lights live at root scope, never inside a group");
                assert_eq!(*node_doc_id, 60);
                assert_eq!(param_id, "intensity");
                assert!(*value > 2.5, "dragging right increases intensity from its 2.5 start");
            }
            other => panic!("expected SceneSetupParamChanged, got {other:?}"),
        }
    }

    #[test]
    fn camera_section_renders_orbit_rows_and_lens_sub_section() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Camera);
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        // Orbit/Tilt/Distance/FOV (4) + Lens's 4 fields = 8 camera value cells.
        assert_eq!(panel.camera_value_cells.len(), 8, "4 orbit rows + 4 lens rows");
        assert!(
            panel.camera_value_cells.iter().any(|(_, row)| row.addr.node_doc_id == 70 && row.addr.param_id == "orbit"),
            "orbit atom's own param resolves"
        );
        assert!(
            panel.camera_value_cells.iter().any(|(_, row)| row.addr.node_doc_id == 71 && row.addr.param_id == "f_stop"),
            "lens pass-through param resolves beneath the camera atom's own rows"
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

    #[test]
    fn free_and_look_at_camera_shapes_render_their_own_rows() {
        let free_cam = CameraRowVm::Free(Box::new(FreeCameraRowVm {
            pos: triplet(70, 1.0, 2.0, 3.0, -1000.0, 1000.0),
            yaw: RowValue { addr: RowAddr::root(70, "yaw"), value: 0.0, min: -6.28, max: 6.28, driven: false, exposed: false },
            pitch: RowValue { addr: RowAddr::root(70, "pitch"), value: 0.0, min: -1.5, max: 1.5, driven: false, exposed: false },
            roll: RowValue { addr: RowAddr::root(70, "roll"), value: 0.0, min: -6.28, max: 6.28, driven: false, exposed: false },
            fov_y: RowValue { addr: RowAddr::root(70, "fov_y"), value: 0.9, min: 0.05, max: 2.5, driven: false, exposed: false },
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
        // Position triplet (3) + yaw + pitch + roll + fov = 7 value cells, no lens.
        assert_eq!(panel.camera_value_cells.len(), 7);

        let look_at_cam = CameraRowVm::LookAt(Box::new(LookAtCameraRowVm {
            pos: triplet(70, 1.0, 2.0, 3.0, -1000.0, 1000.0),
            target: triplet(70, 0.0, 0.0, 0.0, -1000.0, 1000.0),
            fov_y: RowValue { addr: RowAddr::root(70, "fov_y"), value: 0.9, min: 0.05, max: 2.5, driven: false, exposed: false },
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
        // Position triplet (3) + Target triplet (3) + fov = 7 value cells.
        assert_eq!(panel2.camera_value_cells.len(), 7);
    }

    /// SCENE_OBJECT_AND_PANEL_V2_DESIGN.md §4 invariant: every drag-armable
    /// value cell built by the dock is ALSO in the type-in registration set,
    /// and vice versa. PointerDown and DoubleClick resolve through the exact
    /// same lookup in `handle_event` (`value_label_row_at`/`row_value_for` +
    /// the `object`/`light`/`camera_value_cells` chain), so this drives both
    /// gestures on every cell built by the azalea fixture and asserts both
    /// are consumed.
    #[test]
    fn dock_numeric_cells_register_full_contract() {
        // Two passes — Object selected (default) then Light selected — so
        // the invariant covers both properties bodies, not just whichever
        // happens to be the default selection.
        for selection in [None, Some(SceneSelection::Light(60))] {
            let mut panel = ScenePanel::new();
            panel.open();
            panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
            if let Some(sel) = selection {
                panel.selection.insert(LayerId::new("layer-1"), sel);
            }
            let mut tree = UITree::new();
            panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

            let mut cell_ids: Vec<NodeId> = panel.row_ids.iter().filter_map(|ids| ids.value).collect();
            cell_ids.extend(panel.object_value_cells.iter().map(|(id, _)| *id));
            cell_ids.extend(panel.light_value_cells.iter().map(|(id, _)| *id));
            cell_ids.extend(panel.camera_value_cells.iter().map(|(id, _)| *id));
            assert!(!cell_ids.is_empty(), "azalea fixture must exercise at least one drag-armable cell");

            for id in cell_ids {
                let (drag_consumed, _) = panel.handle_event(&UIEvent::PointerDown {
                    node_id: id,
                    pos: Vec2::ZERO,
                    modifiers: crate::input::Modifiers::default(),
                });
                assert!(drag_consumed, "cell {id:?} must be drag-armable");
                panel.handle_event(&UIEvent::PointerUp { node_id: Some(id), pos: Vec2::ZERO });

                let (typein_consumed, actions) = panel.handle_event(&UIEvent::DoubleClick {
                    node_id: id,
                    pos: Vec2::ZERO,
                    modifiers: crate::input::Modifiers::default(),
                });
                assert!(typein_consumed, "cell {id:?} must also open type-in (registration parity)");
                assert!(
                    matches!(actions.as_slice(), [PanelAction::SceneSetupBeginNumericTextInput { .. }]),
                    "double-click must emit SceneSetupBeginNumericTextInput, got {actions:?}"
                );
            }
        }
    }

    /// P4, D8: Shift held at drag-start makes the applied delta 0.1× the
    /// unmodified rate — the performer's precision-landing gesture. Drives
    /// the SAME pixel travel with and without Shift and asserts the ratio.
    #[test]
    fn shift_drag_applies_a_tenth_the_delta() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (light_id, light_row) = panel
            .light_value_cells
            .iter()
            .find(|(_, row)| row.addr.param_id == "intensity")
            .cloned()
            .expect("azalea fixture has a light intensity cell");

        // Coarse: no Shift.
        panel.handle_event(&UIEvent::PointerDown {
            node_id: light_id,
            pos: Vec2::new(0.0, 0.0),
            modifiers: crate::input::Modifiers::default(),
        });
        let (_, coarse_actions) =
            panel.handle_event(&UIEvent::Drag { node_id: Some(light_id), pos: Vec2::new(20.0, 0.0), delta: Vec2::ZERO });
        panel.handle_event(&UIEvent::PointerUp { node_id: Some(light_id), pos: Vec2::new(20.0, 0.0) });
        let PanelAction::SceneSetupParamChanged(.., coarse_value) = &coarse_actions[0] else {
            panic!("expected SceneSetupParamChanged");
        };
        let coarse_delta = coarse_value - light_row.value;

        // Fine: Shift held at PointerDown.
        panel.handle_event(&UIEvent::PointerDown {
            node_id: light_id,
            pos: Vec2::new(0.0, 0.0),
            modifiers: crate::input::Modifiers { shift: true, ..Default::default() },
        });
        let (_, fine_actions) =
            panel.handle_event(&UIEvent::Drag { node_id: Some(light_id), pos: Vec2::new(20.0, 0.0), delta: Vec2::ZERO });
        panel.handle_event(&UIEvent::PointerUp { node_id: Some(light_id), pos: Vec2::new(20.0, 0.0) });
        let PanelAction::SceneSetupParamChanged(.., fine_value) = &fine_actions[0] else {
            panic!("expected SceneSetupParamChanged");
        };
        let fine_delta = fine_value - light_row.value;

        assert!(
            (fine_delta - coarse_delta * 0.1).abs() < 1e-4,
            "fine delta ({fine_delta}) must be 0.1x the coarse delta ({coarse_delta})"
        );
    }

    /// P4, D9: clicking a 3+-label enum value cell (shadow_softness, 4
    /// labels) emits `SceneSetupEnumClicked` with the full label set; a
    /// 2-label row (`mode`) stays a dead click on the VALUE cell (its
    /// `[-]/[+]` steppers already cycle it — `light_steppers` covers that,
    /// unchanged by this phase).
    #[test]
    fn shadow_softness_value_cell_click_opens_dropdown_mode_does_not() {
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(azalea_shaped_vm())));
        panel.selection.insert(LayerId::new("layer-1"), SceneSelection::Light(60));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));

        let (softness_id, _, softness_labels) = panel
            .light_enum_cells
            .iter()
            .find(|(_, row, _)| row.addr.param_id == "shadow_softness")
            .cloned()
            .expect("azalea fixture has a shadow_softness enum cell");
        assert_eq!(softness_labels.len(), 4);
        let (consumed, actions) = panel.handle_event(&UIEvent::Click {
            node_id: softness_id,
            pos: Vec2::ZERO,
            modifiers: crate::input::Modifiers::default(),
        });
        assert!(consumed);
        assert!(
            matches!(actions.as_slice(), [PanelAction::SceneSetupEnumClicked { labels, .. }] if labels.len() == 4),
            "shadow_softness click must open the dropdown with all 4 labels, got {actions:?}"
        );

        let (mode_id, _, mode_labels) = panel
            .light_enum_cells
            .iter()
            .find(|(_, row, _)| row.addr.param_id == "mode")
            .cloned()
            .expect("azalea fixture has a mode enum cell");
        assert_eq!(mode_labels.len(), 2);
        let (_, actions) = panel.handle_event(&UIEvent::Click {
            node_id: mode_id,
            pos: Vec2::ZERO,
            modifiers: crate::input::Modifiers::default(),
        });
        assert!(actions.is_empty(), "a 2-label enum cell's VALUE click stays a dead stop (steppers cycle it)");
    }

    /// P4, D10: the rotation triplet displays degrees (`%.1f°`), not the
    /// stored radians — the conversion happens ONLY at this display
    /// boundary (the underlying `RowValue.value` stays radians).
    #[test]
    fn rotation_triplet_displays_degrees_not_radians() {
        let mut vm = azalea_shaped_vm();
        if let ObjectRowVm::Known(row) = &mut vm.objects[0]
            && let Some(t) = &mut row.transform
        {
            // The shared `triplet()` fixture helper stamps a generic "x"/"y"/"z"
            // param_id for every triplet (pos/rot/scale alike) — production
            // (`scene_vm.rs`) distinguishes `rot_x`/`rot_y`/`rot_z`; fix the
            // addr up so this test exercises the real degrees-row name.
            t.rot.0.addr.param_id = "rot_x".to_string();
            t.rot.0.value = std::f32::consts::FRAC_PI_2; // 90°
        }
        let mut panel = ScenePanel::new();
        panel.open();
        panel.configure(SceneSetupState::Live(Box::new(vm)));
        let mut tree = UITree::new();
        panel.build_docked(&mut tree, Rect::new(0.0, 0.0, 400.0, 800.0));
        let (rot_x_id, rot_x_row) = panel
            .object_value_cells
            .iter()
            .find(|(_, row)| row.addr.param_id == "rot_x")
            .cloned()
            .expect("azalea fixture has a rot_x cell");
        assert!((rot_x_row.value - std::f32::consts::FRAC_PI_2).abs() < 1e-4, "stored value stays radians");
        let text = tree.get_node(rot_x_id).unwrap().text.clone().unwrap();
        assert_eq!(text, "90.0\u{00b0}", "display converts to degrees at the panel boundary");
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
