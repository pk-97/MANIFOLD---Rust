//! Camera: its rig, its lens, and its own depth of field and motion blur,
//! which show while it is the cut. Look band order: depth of field rows 0..10,
//! motion blur rows from 10; each switch leads the rows it controls. The Shake
//! section wobbles the camera about its own position, after the rig.

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Feature, Gate, Modes, Unit};

use super::fields::{
    Choice, FieldControl, FieldRow, FieldValue, always, enabled, is_object_or_group, wrong_value,
};
use super::{Exposure, PropertyRow, Reach, perform, row, setup};
use crate::scene_doc::{CameraRig, ItemFields};

const RIG: Reach = Reach::Own("rig");
const LENS: Reach = Reach::Own("lens");
const COC: Reach = Reach::Own("coc");
const BOKEH: Reach = Reach::Own("bokeh");
const BLUR: Reach = Reach::Own("motion_blur");
const FOCUS: Reach = Reach::Own("focus");
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

/// The closest an Orbit camera comes to what it orbits: at 0 the lens sits
/// on the point it looks at and has no direction to look in.
pub const ORBIT_CLOSEST: f32 = 0.001;

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
    row(property, RIG, Camera, perform(CardBand::Camera, order))
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
}

/// Near and Far each stay above 0: a plane at the lens divides the projection
/// by zero. One micrometre is Blender's own floor. Far stays past Near where
/// the camera is built (`Camera::clip_planes`), so neither row limits the other.
pub const CLIP_PLANE: HardLimit = HardLimit::AtLeast(1e-6);

/// A new camera's Far: 10 km, past the horizon from any camera under about
/// 8 m, so an Ocean runs to the horizon with the default view. The renderer's
/// reversed float depth keeps a far plane this distant as precise as a near
/// one, so nothing else pays for it.
const HORIZON_FAR: f32 = 10_000.0;

const fn clip(property: &'static str) -> PropertyRow {
    row(property, RIG, Camera, setup())
        .unit(Unit::Metres)
        .limit(CLIP_PLANE)
}

const fn place(property: &'static str) -> PropertyRow {
    row(property, RIG, Camera, setup())
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
}

pub static CAMERA_ROWS: [PropertyRow; 67] = [
    // Orbit: a three-quarter view of the origin through a 50 mm lens, framed
    // as the old base scene framed its cube.
    row("orbit", RIG, Camera, perform(CardBand::Camera, 0))
        .limit(HardLimit::Unlimited)
        .default(0.7)
        .when(orbit),
    row("tilt", RIG, Camera, perform(CardBand::Camera, 1))
        .default(0.35)
        .when(orbit),
    row("distance", RIG, Camera, perform(CardBand::Camera, 2))
        .unit(Unit::Metres)
        .range(ORBIT_CLOSEST, 100.0)
        .limit(HardLimit::AtLeast(ORBIT_CLOSEST))
        .default(10.0)
        .when(orbit),
    row("look_y", RIG, Camera, perform(CardBand::Camera, 3))
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
        .when(orbit),
    row("fov_y", RIG, Camera, perform(CardBand::Camera, 4))
        .limit(FIELD_OF_VIEW)
        .default(FIFTY_MM)
        .when(orbit),
    row("roll", RIG, Camera, setup())
        .limit(HardLimit::Unlimited)
        .when(orbit),
    clip("near").when(orbit),
    clip("far").default(HORIZON_FAR).when(orbit),
    // Free.
    row("yaw", RIG, Camera, perform(CardBand::Camera, 0))
        .limit(HardLimit::Unlimited)
        .when(free),
    row("pitch", RIG, Camera, perform(CardBand::Camera, 1)).when(free),
    metres("pos_x", 2).when(free),
    metres("pos_y", 3).when(free),
    metres("pos_z", 4).when(free),
    row("fov_y", RIG, Camera, perform(CardBand::Camera, 5))
        .limit(FIELD_OF_VIEW)
        .default(FIFTY_MM)
        .when(free),
    row("roll", RIG, Camera, setup())
        .limit(HardLimit::Unlimited)
        .when(free),
    clip("near").when(free),
    clip("far").default(HORIZON_FAR).when(free),
    // Look At. With a Target item picked, Target X/Y/Z offset from its origin.
    row("fov_y", RIG, Camera, perform(CardBand::Camera, 0))
        .limit(FIELD_OF_VIEW)
        .default(FIFTY_MM)
        .when(look_at),
    place("pos_x").when(look_at),
    place("pos_y").when(look_at),
    place("pos_z").when(look_at),
    place("target_x").when(look_at),
    place("target_y").when(look_at),
    place("target_z").when(look_at),
    clip("near").when(look_at),
    clip("far").default(HORIZON_FAR).when(look_at),
    // Depth of field, its auto focus and motion blur, all on in a new scene;
    // each switch sits above the rows it gates.
    row(
        "dof_enabled",
        BOKEH,
        DepthOfField,
        perform(CardBand::Look, 0),
    )
    .param("enabled")
    .switch(Feature::DepthOfField)
    .default(1.0),
    row(
        "auto_focus",
        FOCUS,
        DepthOfField,
        perform(CardBand::Look, 1),
    )
    .param("auto")
    .switch(Feature::AutoFocus)
    .gate(Gate::Fold(Feature::DepthOfField))
    .default(1.0),
    // Read only while Auto Focus is off.
    row(
        "focus_distance",
        LENS,
        DepthOfField,
        perform(CardBand::Look, 2),
    )
    .gate(Gate::Only(Feature::AutoFocus, Modes(0b01)))
    .unit(Unit::Metres)
    .range(NEAREST_FOCUS, 100.0)
    .limit(HardLimit::AtLeast(NEAREST_FOCUS))
    .default(10.0),
    row("f_stop", LENS, DepthOfField, perform(CardBand::Look, 3))
        .gate(Gate::Fold(Feature::DepthOfField))
        .limit(HardLimit::AtLeast(0.5)),
    row(
        "motion_blur_enabled",
        BLUR,
        MotionBlur,
        perform(CardBand::Look, 10),
    )
    .param("enabled")
    .switch(Feature::MotionBlur)
    .default(1.0),
    row(
        "shutter_angle",
        LENS,
        MotionBlur,
        perform(CardBand::Look, 11),
    )
    .gate(Gate::Fold(Feature::MotionBlur))
    .limit(HardLimit::Between(0.0, 360.0)),
    row(
        "exposure_ev",
        LENS,
        Camera,
        Exposure::Automatic("the Environment's Exposure"),
    ),
    row(
        "max_blur",
        BOKEH,
        DepthOfField,
        Exposure::Automatic("scales with the frame; Focus and F-Stop set the blur"),
    )
    .param("max_radius"),
    row("aperture", BOKEH, DepthOfField, setup()),
    row("quality", BOKEH, DepthOfField, setup()),
    row(
        "blur_alpha",
        BOKEH,
        DepthOfField,
        Exposure::Automatic("on, so transparent edges blur with the scene"),
    ),
    row(
        "coc_radius",
        COC,
        DepthOfField,
        Exposure::Automatic("matches the bokeh's largest blur"),
    )
    .param("max_radius"),
    row(
        "world_to_mm",
        COC,
        DepthOfField,
        Exposure::Automatic("one scene unit is one metre"),
    ),
    row(
        "max_blur_px",
        BLUR,
        MotionBlur,
        Exposure::Automatic("scales with the frame; Shutter Angle sets the blur"),
    ),
    // Shake: nothing moves on its own, Amount 0 leaves the camera still.
    row("shake_amount", SHAKE, Shake, perform(CardBand::Camera, 20))
        .param("amount")
        .default(0.0),
    row("shake_speed", SHAKE, Shake, perform(CardBand::Camera, 21))
        .param("frequency")
        .name("Speed")
        .default(1.0),
    row(
        "shake_position",
        SHAKE,
        Shake,
        perform(CardBand::Camera, 22),
    )
    .param("position_strength")
    .default(0.25),
    row(
        "shake_rotation",
        SHAKE,
        Shake,
        perform(CardBand::Camera, 23),
    )
    .param("rotation_strength")
    .default(1.0),
    row(
        "shake_time",
        SHAKE,
        Shake,
        Exposure::Automatic("the playback clock"),
    )
    .param("time"),
    // Loop: the flight through the corridor; the corridor itself (its
    // spacing, pattern, stride, start and clipping) and the loop length are
    // the Scene Loop card's.
    row("height", RIG, Camera, perform(CardBand::Camera, 0))
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
        .default(0.0)
        .tip("Camera height inside the tunnel")
        .when(loop_rig),
    row("lateral", RIG, Camera, perform(CardBand::Camera, 1))
        .unit(Unit::Metres)
        .limit(HardLimit::Unlimited)
        .tip("Camera lateral offset inside the tunnel")
        .when(loop_rig),
    row("fov_y", RIG, Camera, perform(CardBand::Camera, 2))
        .limit(FIELD_OF_VIEW)
        .tip("Vertical field of view")
        .when(loop_rig),
    row("yaw", RIG, Camera, perform(CardBand::Camera, 3))
        .tip("Camera yaw")
        .when(loop_rig),
    row("pitch", RIG, Camera, perform(CardBand::Camera, 4))
        .tip("Camera pitch")
        .when(loop_rig),
    row("roll", RIG, Camera, perform(CardBand::Camera, 5))
        .tip("Camera roll angle inside the tunnel")
        .when(loop_rig),
    row("flow", RIG, Camera, perform(CardBand::Camera, 6))
        .tip("Ease camera speed within each loop")
        .when(loop_rig),
    row("sway_amp", RIG, Camera, perform(CardBand::Camera, 7))
        .name("Sway")
        .tip("Camera drift across the tunnel cross-section")
        .when(loop_rig),
    row("sway_cycles", RIG, Camera, perform(CardBand::Camera, 8))
        .name("Sway Rate")
        .tip("Sway cycles per loop")
        .when(loop_rig),
    row("look_sweep_amp", RIG, Camera, perform(CardBand::Camera, 9))
        .name("Look Sway")
        .tip("Side-to-side camera aim sweep")
        .when(loop_rig),
    row("zoom_pulse_amp", RIG, Camera, perform(CardBand::Camera, 10))
        .name("Zoom Pulse")
        .tip("Field-of-view pulse per loop")
        .when(loop_rig),
    row(
        "look_sweep_cycles",
        RIG,
        Camera,
        Exposure::Internal("the look sway turns once a loop"),
    )
    .when(loop_rig),
    row(
        "axis",
        RIG,
        Camera,
        Exposure::Internal("the corridor runs along Z"),
    )
    .when(loop_rig),
    row("cell_size", RIG, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    row(
        "pattern_length",
        RIG,
        Camera,
        Exposure::Automatic(FROM_LOOP),
    )
    .when(loop_rig),
    row(
        "patterns_per_loop",
        RIG,
        Camera,
        Exposure::Automatic(FROM_LOOP),
    )
    .when(loop_rig),
    row("home", RIG, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    clip("near").when(loop_rig),
    clip("far").default(HORIZON_FAR).when(loop_rig),
    // The loop's clock: one even ramp from 0 to 1 per loop of Beats.
    row("bars", PHASE, Camera, Exposure::Automatic(FROM_LOOP)).when(loop_rig),
    row(
        "rate",
        PHASE,
        Camera,
        Exposure::Internal("Beats sets the loop's rate"),
    )
    .when(loop_rig),
    row(
        "attack",
        PHASE,
        Camera,
        Exposure::Internal("one even ramp per loop"),
    )
    .default(1.0)
    .when(loop_rig),
];

const RIGS: &[Choice] = &[
    Choice {
        key: "orbit",
        label: "Orbit",
    },
    Choice {
        key: "free",
        label: "Free",
    },
    Choice {
        key: "look_at",
        label: "Look At",
    },
    Choice {
        key: "loop",
        label: "Loop",
    },
];

pub static CAMERA_FIELDS: [FieldRow; 2] = [
    FieldRow {
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
    },
    FieldRow {
        id: "target",
        label: "Target",
        section: ControlGroup::Camera,
        control: FieldControl::Item {
            items: is_object_or_group,
            none: "Point",
        },
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
    },
];

/// Why the camera's `field` cannot take `key` right now: the Loop rig rides
/// the scene's Scene Loop, so it waits for one.
pub(super) fn choice_unavailable(
    doc: &crate::scene_doc::SceneDoc,
    field: &str,
    key: &str,
) -> Option<&'static str> {
    (field == "rig" && key == "loop" && super::scene_loop(doc).is_none())
        .then_some(super::NO_SCENE_LOOP)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn limits(property: &str) -> Vec<HardLimit> {
        CAMERA_ROWS
            .iter()
            .filter(|row| row.property == property)
            .filter_map(|row| match row.exposure {
                Exposure::Card(facts) => Some(facts.limit),
                _ => None,
            })
            .collect()
    }

    /// The camera rows stop only where the maths does: a 1 cm orbit, a Near
    /// past 5 m and a Far under 10 m all hold, and nothing reaches 0.
    #[test]
    fn camera_rows_stop_only_above_zero() {
        for limit in limits("distance") {
            assert_eq!(limit.clamp(0.01, (0.0, 0.0)), 0.01);
            assert!(limit.clamp(0.0, (0.0, 0.0)) > 0.0);
        }
        let clips: Vec<HardLimit> = limits("near").into_iter().chain(limits("far")).collect();
        assert_eq!(
            clips.len(),
            8,
            "Near and Far on the Orbit, Free, Look At and Loop rigs"
        );
        for limit in clips {
            for metres in [0.01, 6.0, 30.0, 50_000.0] {
                assert_eq!(limit.clamp(metres, (0.0, 0.0)), metres);
            }
            assert!(limit.clamp(0.0, (0.0, 0.0)) > 0.0);
        }
    }
}
