//! Camera: its rig, its lens, and the depth of field and motion blur that
//! follow the active camera. Look band order: depth of field rows 0..10,
//! motion blur rows from 10; the Shake section is its own group; each switch leads the rows it controls.

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
const SHAKE_FRAME: Reach = Reach::Own("shake_frame");

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

const fn metres(property: &'static str, order: u8) -> PropertyRow {
    row(property, RIG, Camera, perform(CardBand::Camera, order)).unit(Unit::Metres).limit(HardLimit::Unlimited)
}

/// Near stays above 0 and at most 5 m; Far stays at least 10 m. The two ranges
/// do not overlap, so Near is always below Far whichever row moves.
const NEAR_CLIP: HardLimit = HardLimit::Between(0.001, 5.0);
const FAR_CLIP: HardLimit = HardLimit::AtLeast(10.0);

const fn clip(property: &'static str, limit: HardLimit) -> PropertyRow {
    row(property, RIG, Camera, setup()).unit(Unit::Metres).limit(limit)
}

const fn shake_frame(property: &'static str, param: &'static str) -> PropertyRow {
    row(property, SHAKE_FRAME, Shake, Exposure::Automatic("an identity frame the shake moves")).param(param)
}

const fn place(property: &'static str) -> PropertyRow {
    row(property, RIG, Camera, setup()).unit(Unit::Metres).limit(HardLimit::Unlimited)
}

pub static CAMERA_ROWS: [PropertyRow; 54] = [
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
    // The identity frame the shake moves; a group the camera sits in joins it later.
    shake_frame("shake_frame_pos_x", "pos_x"),
    shake_frame("shake_frame_pos_y", "pos_y"),
    shake_frame("shake_frame_pos_z", "pos_z"),
    shake_frame("shake_frame_rot_x", "rot_x"),
    shake_frame("shake_frame_rot_y", "rot_y"),
    shake_frame("shake_frame_rot_z", "rot_z"),
    shake_frame("shake_frame_scale_x", "scale_x"),
    shake_frame("shake_frame_scale_y", "scale_y"),
    shake_frame("shake_frame_scale_z", "scale_z"),
    shake_frame("shake_frame_billboard", "billboard"),
];

const RIGS: &[Choice] = &[
    Choice { key: "orbit", label: "Orbit" },
    Choice { key: "free", label: "Free" },
    Choice { key: "look_at", label: "Look At" },
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
