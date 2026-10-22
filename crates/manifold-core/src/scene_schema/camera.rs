//! Camera: its rig, its lens, and its own depth of field and motion blur,
//! which show while it is the cut. Look band order: depth of field rows 0..10,
//! motion blur rows from 10; each switch leads the rows it controls. The Shake
//! section wobbles the camera about its own position, after the rig.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Feature, Gate, Unit};

use super::fields::{Choice, FieldControl, FieldRow, FieldValue, always, enabled, is_object_or_group, wrong_value};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{CameraRig, ItemFields};

const RIG: Reach = Reach::Own("rig");
const LENS: Reach = Reach::Own("lens");
const COC: Reach = Reach::Own("coc");
const BOKEH: Reach = Reach::Own("bokeh");
const BLUR: Reach = Reach::Own("motion_blur");
use ControlGroup::{Camera, DepthOfField, MotionBlur, Shake};
const SHAKE: Reach = Reach::Own("shake");
const PHASE: Reach = Reach::Own("phase");

/// What sets the Loop rig's corridor params.
const FROM_LOOP: &str = "the scene's Scene Loop";

/// Field of view, 1° to 179°, in radians.
const FIELD_OF_VIEW: HardLimit = HardLimit::Between(0.017_453_292, 3.124_139_4);

/// The vertical field of view of a 50 mm lens on a full-frame sensor
/// (24 mm tall): 2·atan(12/50).
const FIFTY_MM: f32 = 0.471_239;

/// The closest an Orbit camera comes to what it orbits: nearer, the lens
/// sits inside the object at the pivot and the view goes black.
const CLOSEST: f32 = 1.0;

/// The nearest Focus Distance. The lens reads 0 as "no depth of field", so
/// a card that reached it would switch the blur off behind the switch.
const NEAREST_FOCUS: f32 = 0.1;

fn orbit(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Camera(c) if c.rig == CameraRig::Orbit)
}

fn free(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Camera(c) if c.rig == CameraRig::Free)
}

fn look_at(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Camera(c) if c.rig == CameraRig::LookAt)
}

fn loop_rig(fields: &ItemFields) -> bool {
    matches!(fields, ItemFields::Camera(c) if c.rig == CameraRig::Loop)
}

const fn metres(property: &'static str, order: u8) -> PropertyRow {
    row(property, RIG, Camera, perform(CardBand::Camera, order)).unit(Unit::Metres).limit(HardLimit::Unlimited)
}

/// Near stays above 0 and at most 5 m; Far stays at least 10 m. The two ranges
/// do not overlap, so Near is always below Far whichever row moves.
pub const NEAR_CLIP: HardLimit = HardLimit::Between(0.001, 5.0);
pub const FAR_CLIP: HardLimit = HardLimit::AtLeast(10.0);

const fn clip(property: &'static str, limit: HardLimit) -> PropertyRow {
    row(property, RIG, Camera, setup()).unit(Unit::Metres).limit(limit)
}

const fn place(property: &'static str) -> PropertyRow {
    row(property, RIG, Camera, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited)
}

pub static CAMERA_ROWS: [PropertyRow; 66] = [
    // Orbit: a three-quarter view of the origin through a 50 mm lens, framed
    // as the old base scene framed its cube.
    row("orbit", RIG, Camera, perform(CardBand::Camera, 0)).limit(HardLimit::Unlimited).default(0.7).when(orbit),
    row("tilt", RIG, Camera, perform(CardBand::Camera, 1)).default(0.35).when(orbit),
    row("distance", RIG, Camera, perform(CardBand::Camera, 2))
        .unit(Unit::Metres)
        .range(CLOSEST, 100.0)
        .limit(HardLimit::AtLeast(CLOSEST))
        .default(10.0)
        .when(orbit),
    row("look_y", RIG, Camera, perform(CardBand::Camera, 3)).unit(Unit::Metres).limit(HardLimit::Unlimited).when(orbit),
    row("fov_y", RIG, Camera, perform(CardBand::Camera, 4)).limit(FIELD_OF_VIEW).default(FIFTY_MM).when(orbit),
    row("roll", RIG, Camera, setup()).limit(HardLimit::Unlimited).when(orbit),
    clip("near", NEAR_CLIP).when(orbit),
    clip("far", FAR_CLIP).when(orbit),
    // Free.
    row("yaw", RIG, Camera, perform(CardBand::Camera, 0)).limit(HardLimit::Unlimited).when(free),
    row("pitch", RIG, Camera, perform(CardBand::Camera, 1)).when(free),
    metres("pos_x", 2).when(free),
    metres("pos_y", 3).when(free),
    metres("pos_z", 4).when(free),
    row("fov_y", RIG, Camera, perform(CardBand::Camera, 5)).limit(FIELD_OF_VIEW).default(FIFTY_MM).when(free),
    row("roll", RIG, Camera, setup()).limit(HardLimit::Unlimited).when(free),
    clip("near", NEAR_CLIP).when(free),
    clip("far", FAR_CLIP).when(free),
    // Look At. With a Target item picked, Target X/Y/Z offset from its origin.
    row("fov_y", RIG, Camera, perform(CardBand::Camera, 0)).limit(FIELD_OF_VIEW).default(FIFTY_MM).when(look_at),
    place("pos_x").when(look_at),
    place("pos_y").when(look_at),
    place("pos_z").when(look_at),
    place("target_x").when(look_at),
    place("target_y").when(look_at),
    place("target_z").when(look_at),
    clip("near", NEAR_CLIP).when(look_at),
    clip("far", FAR_CLIP).when(look_at),
    // Lens.
    row("focus_distance", LENS, DepthOfField, perform(CardBand::Look, 1))
        .gate(Gate::Dim(Feature::DepthOfField))
        .unit(Unit::Metres)
        .range(NEAREST_FOCUS, 100.0)
        .limit(HardLimit::AtLeast(NEAREST_FOCUS))
        .default(10.0),
    row("f_stop", LENS, DepthOfField, perform(CardBand::Look, 2))
        .gate(Gate::Fold(Feature::DepthOfField))
        .limit(HardLimit::AtLeast(0.5)),
    row("shutter_angle", LENS, MotionBlur, perform(CardBand::Look, 11))
        .gate(Gate::Fold(Feature::MotionBlur))
        .limit(HardLimit::Between(0.0, 360.0)),
    row("exposure_ev", LENS, Camera, Exposure::Automatic("the Environment's Exposure")),
    // Depth of field and motion blur, both off in a new scene.
    row("dof_enabled", BOKEH, DepthOfField, perform(CardBand::Look, 0))
        .param("enabled")
        .switch(Feature::DepthOfField)
        .default(0.0),
    row("max_blur", BOKEH, DepthOfField, perform(CardBand::Look, 3))
        .param("max_radius")
        .gate(Gate::Fold(Feature::DepthOfField))
        .unit(Unit::Pixels)
        .limit(AT_LEAST_ZERO),
    row("aperture", BOKEH, DepthOfField, setup()),
    row("quality", BOKEH, DepthOfField, setup()),
    row("blur_alpha", BOKEH, DepthOfField, Exposure::Automatic("on, so transparent edges blur with the scene")),
    row("coc_radius", COC, DepthOfField, Exposure::Automatic("matches the bokeh's largest blur")).param("max_radius"),
    row("world_to_mm", COC, DepthOfField, Exposure::Automatic("one scene unit is one metre")),
    row("motion_blur_enabled", BLUR, MotionBlur, perform(CardBand::Look, 10))
        .param("enabled")
        .switch(Feature::MotionBlur)
        .default(0.0),
    row("max_blur_px", BLUR, MotionBlur, perform(CardBand::Look, 12))
        .gate(Gate::Fold(Feature::MotionBlur))
        .unit(Unit::Pixels)
        .limit(AT_LEAST_ZERO),
    // Shake: nothing moves on its own, Amount 0 leaves the camera still.
    row("shake_amount", SHAKE, Shake, perform(CardBand::Camera, 20)).param("amount").default(0.0),
    row("shake_speed", SHAKE, Shake, perform(CardBand::Camera, 21)).param("frequency").name("Speed").default(1.0),
    row("shake_position", SHAKE, Shake, perform(CardBand::Camera, 22)).param("position_strength").default(0.25),
    row("shake_rotation", SHAKE, Shake, perform(CardBand::Camera, 23)).param("rotation_strength").default(1.0),
    row("shake_time", SHAKE, Shake, Exposure::Automatic("the playback clock")).param("time"),
    // Loop: the flight through the corridor; the corridor itself (its
    // spacing, pattern, stride, start and clipping) and the loop length are
    // the Scene Loop card's.
    row("height", RIG, Camera, perform(CardBand::Camera, 0)).unit(Unit::Metres).limit(HardLimit::Unlimited).default(0.0).when(loop_rig),
    row("lateral", RIG, Camera, perform(CardBand::Camera, 1)).unit(Unit::Metres).limit(HardLimit::Unlimited).when(loop_rig),
    row("fov_y", RIG, Camera, perform(CardBand::Camera, 2)).limit(FIELD_OF_VIEW).when(loop_rig),
    row("yaw", RIG, Camera, perform(CardBand::Camera, 3)).when(loop_rig),
    row("pitch", RIG, Camera, perform(CardBand::Camera, 4)).when(loop_rig),
    row("roll", RIG, Camera, perform(CardBand::Camera, 5)).when(loop_rig),
    row("flow", RIG, Camera, perform(CardBand::Camera, 6)).when(loop_rig),
    row("sway_amp", RIG, Camera, perform(CardBand::Camera, 7)).name("Sway").when(loop_rig),
    row("sway_cycles", RIG, Camera, perform(CardBand::Camera, 8)).name("Sway Rate").when(loop_rig),
    row("look_sweep_amp", RIG, Camera, perform(CardBand::Camera, 9)).name("Look Sway").when(loop_rig),
    row("zoom_pulse_amp", RIG, Camera, perform(CardBand::Camera, 10)).name("Zoom Pulse").when(loop_rig),
    row("look_sweep_cycles", RIG, Camera, Exposure::Internal("the look sway turns once a loop")).when(loop_rig),
    row("axis", RIG, Camera, Exposure::Internal("the corridor runs along Z")).when(loop_rig),
    row("cell_size", RIG, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    row("pattern_length", RIG, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    row("patterns_per_loop", RIG, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    row("home", RIG, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    row("near", RIG, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    row("far", RIG, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    // The loop's clock: one even ramp from 0 to 1 per loop of Beats.
    row("bars", PHASE, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    row("rate", PHASE, Camera, Exposure::Internal("Beats sets the loop's rate")).when(loop_rig),
    row("attack", PHASE, Camera, Exposure::Internal("one even ramp per loop")).default(1.0).when(loop_rig),
];

const RIGS: &[Choice] = &[
    Choice { key: "orbit", label: "Orbit" },
    Choice { key: "free", label: "Free" },
    Choice { key: "look_at", label: "Look At" },
    Choice { key: "loop", label: "Loop" },
];

pub static CAMERA_FIELDS: [FieldRow; 2] = [FieldRow {
    id: "rig",
    label: "Rig",
    section: ControlGroup::Camera,
    control: FieldControl::Closed(RIGS),
    applies: always,
    disabled: enabled,
    read: |fields| match fields {
        ItemFields::Camera(c) => FieldValue::Choice(
            match c.rig {
                CameraRig::Orbit => "orbit",
                CameraRig::Free => "free",
                CameraRig::LookAt => "look_at",
                CameraRig::Loop => "loop",
            }
            .to_owned(),
        ),
        _ => FieldValue::Choice(String::new()),
    },
    write: |fields, value| {
        let (ItemFields::Camera(c), FieldValue::Choice(key)) = (fields, value) else {
            return Err(wrong_value("rig", value));
        };
        c.rig = match key.as_str() {
            "orbit" => CameraRig::Orbit,
            "free" => CameraRig::Free,
            "look_at" => CameraRig::LookAt,
            "loop" => CameraRig::Loop,
            _ => return Err(wrong_value("rig", value)),
        };
        Ok(())
    },
}, FieldRow {
    id: "target",
    label: "Target",
    section: ControlGroup::Camera,
    control: FieldControl::Item { items: is_object_or_group, none: "Point" },
    applies: look_at,
    disabled: enabled,
    read: |fields| match fields {
        ItemFields::Camera(c) => FieldValue::Item(c.target.clone()),
        _ => FieldValue::Item(None),
    },
    write: |fields, value| match (fields, value) {
        (ItemFields::Camera(c), FieldValue::Item(target)) => {
            c.target = target.clone();
            Ok(())
        }
        _ => Err(wrong_value("target", value)),
    },
}];

/// Why the camera's `field` cannot take `key` right now: the Loop rig rides
/// the scene's Scene Loop, so it waits for one.
pub(super) fn choice_unavailable(doc: &crate::scene_doc::SceneDoc, field: &str, key: &str) -> Option<&'static str> {
    (field == "rig" && key == "loop" && super::scene_loop(doc).is_none()).then_some(super::NO_SCENE_LOOP)
}
