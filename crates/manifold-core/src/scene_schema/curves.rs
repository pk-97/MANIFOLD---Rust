//! The curve shapes: Extrude, Revolve and Tube, each made from a built-in
//! curve (SCENE_DOCUMENT_DESIGN.md lane L20). Which curve is a structural
//! field; every curve row and Depth, Sweep, Offset and Thickness are
//! performance rows, so the drawers reshape the solid live. The detail (curve
//! samples, the steps, segments and sides around it) is fixed, so a live row
//! never resizes a mesh a modifier holds.
//!
//! A curve is a chain of curve atoms (`node.range`, `node.array_math`,
//! `node.frequency_ratio`, `node.polygon_curve`) feeding `node.combine_xy`,
//! the sweep node and `node.make_triangles`. The chain's math nodes take
//! generic roles, so one row reaches a different role on each curve.

use std::f32::consts::TAU;

use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{CardBand, ControlGroup, Unit};

use super::fields::{Choice, FieldControl, FieldRow, FieldValue, enabled, is_on, on_off, parse_on, wrong_value};
use super::{AT_LEAST_ZERO, Exposure, PropertyRow, Reach, perform, row};
use crate::scene_doc::{Curve, ItemFields, Shape};

/// The sample sweep every curve but Polygon and Star is computed from.
pub const CURVE_T: &str = "curve_t";
/// Lissajous's harmonic ratio.
pub const CURVE_RATIO: &str = "curve_ratio";
/// Polygon and Star's outline.
pub const CURVE_POLYGON: &str = "curve_polygon";
/// The curve maths, in the order each curve uses them.
pub const CURVE_MATH: [&str; 7] = ["curve_m0", "curve_m1", "curve_m2", "curve_m3", "curve_m4", "curve_m5", "curve_m6"];
/// Revolve's shift of the curve away from the axis.
pub const CURVE_AXIS: &str = "curve_axis";
/// The curve's points.
pub const CURVE_XY: &str = "curve_xy";
/// The extrude, revolve or tube node.
pub const SWEEP: &str = "sweep";
/// The triangles the sweep's grid becomes.
pub const TRIANGLES: &str = "triangles";
/// The solid physics and copies read: the same sweep, on the CPU.
pub const SOLID: &str = "solid";

const M0: Reach = Reach::Own(CURVE_MATH[0]);
const M1: Reach = Reach::Own(CURVE_MATH[1]);
const M2: Reach = Reach::Own(CURVE_MATH[2]);
const M3: Reach = Reach::Own(CURVE_MATH[3]);
const M4: Reach = Reach::Own(CURVE_MATH[4]);
const M5: Reach = Reach::Own(CURVE_MATH[5]);
const M6: Reach = Reach::Own(CURVE_MATH[6]);
const T: Reach = Reach::Own(CURVE_T);
const RATIO: Reach = Reach::Own(CURVE_RATIO);
const POLY: Reach = Reach::Own(CURVE_POLYGON);
const AXIS: Reach = Reach::Own(CURVE_AXIS);
const XY: Reach = Reach::Own(CURVE_XY);
const SW: Reach = Reach::Own(SWEEP);
const TRI: Reach = Reach::Own(TRIANGLES);
const SOLID_OF: Reach = Reach::Own(SOLID);

const SHAPE: ControlGroup = ControlGroup::Shape;
const BAND: CardBand = CardBand::Scene;

const BY_CURVE: &str = "the Curve field";
const DETAIL: &str = "fixed, so a live row never resizes the mesh";
const NO_EXPONENT: &str = "the curve maths uses no exponent or bias";
const CENTRED: &str = "an extrusion is centred on its curve's plane, as Blender's is";
const BY_CAPS: &str = "the Fill Caps field";
const FOLLOWS: &str = "the sweep node's value and row";

fn shape_of(fields: &ItemFields) -> Option<&Shape> {
    match fields {
        ItemFields::Object(o) => Some(&o.shape),
        _ => None,
    }
}

fn curve_of(fields: &ItemFields) -> Option<Curve> {
    shape_of(fields).and_then(Shape::curve)
}

fn is_curve_shape(fields: &ItemFields) -> bool {
    curve_of(fields).is_some()
}

fn extrude(fields: &ItemFields) -> bool {
    matches!(shape_of(fields), Some(Shape::Extrude { .. }))
}

fn revolve(fields: &ItemFields) -> bool {
    matches!(shape_of(fields), Some(Shape::Revolve { .. }))
}

fn tube(fields: &ItemFields) -> bool {
    matches!(shape_of(fields), Some(Shape::Tube { .. }))
}

fn circle(fields: &ItemFields) -> bool {
    curve_of(fields) == Some(Curve::Circle)
}

fn polygon(fields: &ItemFields) -> bool {
    curve_of(fields) == Some(Curve::Polygon)
}

fn star(fields: &ItemFields) -> bool {
    curve_of(fields) == Some(Curve::Star)
}

fn spiral(fields: &ItemFields) -> bool {
    curve_of(fields) == Some(Curve::Spiral)
}

fn helix(fields: &ItemFields) -> bool {
    curve_of(fields) == Some(Curve::Helix)
}

fn spiral_or_helix(fields: &ItemFields) -> bool {
    spiral(fields) || helix(fields)
}

fn wave(fields: &ItemFields) -> bool {
    curve_of(fields) == Some(Curve::Wave)
}

fn lissajous(fields: &ItemFields) -> bool {
    curve_of(fields) == Some(Curve::Lissajous)
}

/// A `node.array_math` the compiler sets up: its op and the scale and offset
/// no row reaches follow the curve.
macro_rules! math_rows {
    ($role:expr, $op:literal, $scale:literal, $offset:literal, $exp:literal, $bias:literal) => {
        [
            row($op, $role, SHAPE, Exposure::Automatic(BY_CURVE)).param("op").when(is_curve_shape),
            row($scale, $role, SHAPE, Exposure::Automatic(BY_CURVE)).param("scale").when(is_curve_shape),
            row($offset, $role, SHAPE, Exposure::Automatic(BY_CURVE)).param("offset").when(is_curve_shape),
            row($exp, $role, SHAPE, Exposure::Internal(NO_EXPONENT)).param("exp").when(is_curve_shape),
            row($bias, $role, SHAPE, Exposure::Internal(NO_EXPONENT)).param("bias").when(is_curve_shape),
        ]
    };
}

static MATH_ROWS: [[PropertyRow; 5]; 8] = [
    math_rows!(M0, "curve_m0_op", "curve_m0_scale", "curve_m0_offset", "curve_m0_exp", "curve_m0_bias"),
    math_rows!(M1, "curve_m1_op", "curve_m1_scale", "curve_m1_offset", "curve_m1_exp", "curve_m1_bias"),
    math_rows!(M2, "curve_m2_op", "curve_m2_scale", "curve_m2_offset", "curve_m2_exp", "curve_m2_bias"),
    math_rows!(M3, "curve_m3_op", "curve_m3_scale", "curve_m3_offset", "curve_m3_exp", "curve_m3_bias"),
    math_rows!(M4, "curve_m4_op", "curve_m4_scale", "curve_m4_offset", "curve_m4_exp", "curve_m4_bias"),
    math_rows!(M5, "curve_m5_op", "curve_m5_scale", "curve_m5_offset", "curve_m5_exp", "curve_m5_bias"),
    math_rows!(M6, "curve_m6_op", "curve_m6_scale", "curve_m6_offset", "curve_m6_exp", "curve_m6_bias"),
    math_rows!(AXIS, "curve_axis_op", "curve_axis_scale", "curve_axis_offset", "curve_axis_exp", "curve_axis_bias"),
];

static PLUMBING_ROWS: [PropertyRow; 27] = [
    row("curve_t_start", T, SHAPE, Exposure::Automatic(BY_CURVE)).param("start").when(is_curve_shape),
    row("curve_t_end", T, SHAPE, Exposure::Automatic(BY_CURVE)).param("end").when(is_curve_shape),
    row("curve_t_samples", T, SHAPE, Exposure::Automatic(DETAIL)).param("max_capacity").when(is_curve_shape),
    row("curve_t_closed", T, SHAPE, Exposure::Automatic("every curve ends where a closed one starts"))
        .param("end_inclusive")
        .when(is_curve_shape),
    row("curve_polygon_samples", POLY, SHAPE, Exposure::Automatic(DETAIL)).param("max_capacity").when(is_curve_shape),
    row("curve_xy_scale", XY, SHAPE, Exposure::Automatic("the curve's points come out at their own size"))
        .param("scale")
        .when(is_curve_shape),
    row("extrude_steps", SW, SHAPE, Exposure::Automatic(DETAIL)).param("steps").when(extrude),
    row("extrude_close", SW, SHAPE, Exposure::Internal("a closed curve already ends on its first point"))
        .param("close")
        .when(extrude),
    row("revolve_segments", SW, SHAPE, Exposure::Automatic(DETAIL)).param("segments").when(revolve),
    row("tube_sides", SW, SHAPE, Exposure::Automatic(DETAIL)).param("sides").when(tube),
    row("triangles_cols", TRI, SHAPE, Exposure::Automatic(DETAIL)).param("src_cols").when(is_curve_shape),
    row("triangles_rows", TRI, SHAPE, Exposure::Automatic(DETAIL)).param("src_rows").when(is_curve_shape),
    row("star_pinch", POLY, SHAPE, Exposure::Internal("a polygon has straight sides")).param("pinch").when(polygon),
    row("extrude_symmetric", SW, SHAPE, Exposure::Automatic(CENTRED)).param("symmetric").when(extrude),
    row("extrude_caps", SW, SHAPE, Exposure::Automatic(BY_CAPS)).param("caps").when(extrude),
    row("tube_caps", SW, SHAPE, Exposure::Automatic(BY_CAPS)).param("caps").when(tube),
    row("triangles_keep_normals", TRI, SHAPE, Exposure::Automatic("the sweep writes exact normals")).param("keep_normals").when(is_curve_shape),
    // The solid follows the sweep node's params and rows.
    row("solid_kind", SOLID_OF, SHAPE, Exposure::Automatic("the Shape field")).param("kind").when(is_curve_shape),
    row("solid_depth", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("depth").when(is_curve_shape),
    row("solid_steps", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("steps").when(is_curve_shape),
    row("solid_close", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("close").when(is_curve_shape),
    row("solid_symmetric", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("symmetric").when(is_curve_shape),
    row("solid_caps", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("caps").when(is_curve_shape),
    row("solid_segments", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("segments").when(is_curve_shape),
    row("solid_sweep", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("sweep").when(is_curve_shape),
    row("solid_radius", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("radius").when(is_curve_shape),
    row("solid_sides", SOLID_OF, SHAPE, Exposure::Automatic(FOLLOWS)).param("sides").when(is_curve_shape),
];

static CARD_ROWS: [PropertyRow; 21] = [
    // The sweep.
    row("depth", SW, SHAPE, perform(BAND, 0)).default(1.0).range(0.0, 4.0).limit(HardLimit::Unlimited).unit(Unit::Metres).when(extrude),
    row("sweep", SW, SHAPE, perform(BAND, 0)).default(TAU).range(0.0, TAU).limit(HardLimit::Unlimited).when(revolve),
    row("offset", AXIS, SHAPE, perform(BAND, 1))
        .param("offset")
        .name("Offset")
        .default(0.6)
        .range(-2.0, 2.0)
        .limit(HardLimit::Unlimited)
        .unit(Unit::Metres)
        .when(revolve),
    row("thickness", SW, SHAPE, perform(BAND, 0))
        .param("radius")
        .name("Thickness")
        .default(0.08)
        .range(0.0, 0.5)
        .limit(AT_LEAST_ZERO)
        .unit(Unit::Metres)
        .when(tube),
    // Circle.
    row("radius", M2, SHAPE, perform(BAND, 2)).param("scale").name("Radius").default(0.5).range(0.0, 2.0).limit(AT_LEAST_ZERO).unit(Unit::Metres).when(circle),
    // Polygon and Star.
    row("sides", POLY, SHAPE, perform(BAND, 2)).name("Sides").default(6.0).when(polygon),
    row("sides", POLY, SHAPE, perform(BAND, 2)).name("Points").default(5.0).when(star),
    row("radius", POLY, SHAPE, perform(BAND, 3)).name("Radius").default(0.5).range(0.0, 2.0).limit(AT_LEAST_ZERO).unit(Unit::Metres).when(polygon),
    row("radius", POLY, SHAPE, perform(BAND, 3)).name("Radius").default(0.5).range(0.0, 2.0).limit(AT_LEAST_ZERO).unit(Unit::Metres).when(star),
    row("pinch", POLY, SHAPE, perform(BAND, 4)).name("Pinch").default(0.5).when(star),
    // Spiral and Helix.
    row("turns", M0, SHAPE, perform(BAND, 2)).param("scale").name("Turns").default(3.0).range(0.0, 10.0).limit(HardLimit::Unlimited).when(spiral_or_helix),
    row("radius", M2, SHAPE, perform(BAND, 3)).param("scale").name("Radius").default(0.5).range(0.0, 2.0).limit(AT_LEAST_ZERO).unit(Unit::Metres).when(spiral),
    row("radius", M4, SHAPE, perform(BAND, 3)).param("scale").name("Radius").default(0.5).range(0.0, 2.0).limit(AT_LEAST_ZERO).unit(Unit::Metres).when(helix),
    row("height", M6, SHAPE, perform(BAND, 4)).param("scale").name("Height").default(1.0).range(0.0, 4.0).limit(HardLimit::Unlimited).unit(Unit::Metres).when(helix),
    // Wave.
    row("length", M0, SHAPE, perform(BAND, 2)).param("scale").name("Length").default(1.0).range(0.0, 4.0).limit(HardLimit::Unlimited).unit(Unit::Metres).when(wave),
    row("frequency", M1, SHAPE, perform(BAND, 3)).param("scale").name("Waves").default(2.0).range(0.0, 10.0).limit(HardLimit::Unlimited).when(wave),
    row("phase", M2, SHAPE, perform(BAND, 4)).param("offset").name("Phase").default(0.0).range(0.0, TAU).limit(HardLimit::Unlimited).when(wave),
    row("amplitude", M4, SHAPE, perform(BAND, 5)).param("scale").name("Amplitude").default(0.15).range(0.0, 1.0).limit(HardLimit::Unlimited).unit(Unit::Metres).when(wave),
    // Lissajous.
    row("ratio", RATIO, SHAPE, perform(BAND, 2)).param("index").name("Ratio").default(2.0).when(lissajous),
    row("phase", M0, SHAPE, perform(BAND, 3)).param("offset").name("Phase").default(0.0).range(0.0, TAU).limit(HardLimit::Unlimited).when(lissajous),
    row("radius", M4, SHAPE, perform(BAND, 4)).param("scale").name("Radius").default(0.5).range(0.0, 2.0).limit(AT_LEAST_ZERO).unit(Unit::Metres).when(lissajous),
];

/// Every row of the curve shapes: the card rows first, so a card row wins
/// over the plumbing row for the same node param.
pub fn curve_rows() -> impl Iterator<Item = &'static PropertyRow> {
    CARD_ROWS.iter().chain(PLUMBING_ROWS.iter()).chain(MATH_ROWS.iter().flatten())
}

/// The curves a curve shape offers.
const CURVES: &[Choice] = &[
    Choice { key: "circle", label: "Circle" },
    Choice { key: "polygon", label: "Polygon" },
    Choice { key: "star", label: "Star" },
    Choice { key: "spiral", label: "Spiral" },
    Choice { key: "helix", label: "Helix" },
    Choice { key: "wave", label: "Wave" },
    Choice { key: "lissajous", label: "Lissajous" },
];

fn curve_key(curve: Curve) -> &'static str {
    match curve {
        Curve::Circle => "circle",
        Curve::Polygon => "polygon",
        Curve::Star => "star",
        Curve::Spiral => "spiral",
        Curve::Helix => "helix",
        Curve::Wave => "wave",
        Curve::Lissajous => "lissajous",
    }
}

fn read_curve(fields: &ItemFields) -> FieldValue {
    FieldValue::Choice(curve_of(fields).map_or("", curve_key).to_owned())
}

fn write_curve(fields: &mut ItemFields, value: &FieldValue) -> Result<(), String> {
    let FieldValue::Choice(key) = value else {
        return Err(wrong_value("curve", value));
    };
    let Some(picked) = [Curve::Circle, Curve::Polygon, Curve::Star, Curve::Spiral, Curve::Helix, Curve::Wave, Curve::Lissajous]
        .into_iter()
        .find(|curve| curve_key(*curve) == key)
    else {
        return Err(wrong_value("curve", value));
    };
    match fields {
        ItemFields::Object(o) => match &mut o.shape {
            Shape::Extrude { curve, .. } | Shape::Revolve { curve } | Shape::Tube { curve, .. } => {
                *curve = picked;
                Ok(())
            }
            _ => Err(wrong_value("curve", value)),
        },
        _ => Err(wrong_value("curve", value)),
    }
}

/// The Curve field of a curve shape.
pub(super) const CURVE_FIELD: FieldRow = FieldRow {
    id: "curve",
    label: "Curve",
    section: SHAPE,
    control: FieldControl::Closed(CURVES),
    applies: is_curve_shape,
    disabled: enabled,
    read: read_curve,
    write: write_curve,
};

fn can_cap(fields: &ItemFields) -> bool {
    shape_of(fields).is_some_and(Shape::can_cap)
}

/// Fill Caps (Blender's): closes an Extrude's outline or a Tube's open ends.
/// Structural: caps add rows to the mesh.
pub(super) const CAPS_FIELD: FieldRow = FieldRow {
    id: "caps",
    label: "Fill Caps",
    section: SHAPE,
    control: on_off(),
    applies: can_cap,
    disabled: enabled,
    read: |fields| is_on(shape_of(fields).is_some_and(Shape::capped)),
    write: |fields, value| {
        let on = parse_on("caps", value)?;
        match fields {
            ItemFields::Object(o) => match &mut o.shape {
                Shape::Extrude { caps, .. } | Shape::Tube { caps, .. } => {
                    *caps = on;
                    Ok(())
                }
                _ => Err(wrong_value("caps", value)),
            },
            _ => Err(wrong_value("caps", value)),
        }
    },
};
