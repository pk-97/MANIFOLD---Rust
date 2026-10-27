//! The shapes a scene thing declares for the viewport gizmo, by the param
//! ids that place it. Where a param lives on the card is the (kind,
//! property) table in `manifold_core::scene_schema`; this file holds only the
//! gizmo declarations. The ids are property names: the scene VM joins an
//! item's id onto them to find the manifest param.

use std::borrow::Cow;

use manifold_core::fluid_domain::MIN_DOMAIN_SIDE_METRES;
use manifold_foundation::param_range::HardLimit;
use manifold_foundation::scene_surface::{ParamTriple, SceneShape, SceneShapeDecl};

const fn id(name: &'static str) -> Cow<'static, str> {
    Cow::Borrowed(name)
}

const POS: ParamTriple = [id("pos_x"), id("pos_y"), id("pos_z")];
const ROT: ParamTriple = [id("rot_x"), id("rot_y"), id("rot_z")];
const SCALE: ParamTriple = [id("scale_x"), id("scale_y"), id("scale_z")];

/// A box placed, turned and sized by an item's own transform rows.
const fn transform_box(rotates: bool) -> SceneShapeDecl {
    SceneShapeDecl {
        shape: SceneShape::Box {
            center: POS,
            rotation: if rotates { Some(ROT) } else { None },
            size: SCALE,
        },
        via: None,
        extent_limit: None,
        uniform_scale: false,
    }
}

/// An Object's or a Group's gizmo: moved, turned and scaled by its transform.
pub static ITEM_SHAPES: [SceneShapeDecl; 1] = [transform_box(true)];

/// A copy group's gizmo: moved and turned as one and scaled by one number,
/// because the Physics World reads a group's scale as uniform.
pub static COPY_GROUP_SHAPES: [SceneShapeDecl; 1] = [{
    let mut decl = transform_box(true);
    decl.uniform_scale = true;
    decl
}];

/// The liquid domain box: each side stays inside what the solvers accept.
const DOMAIN_BOX: SceneShapeDecl = {
    let mut decl = transform_box(false);
    decl.extent_limit = Some(HardLimit::AtLeast(MIN_DOMAIN_SIDE_METRES));
    decl
};

/// The solvers take an axis-aligned box only, so it does not rotate.
pub static LIQUID_DOMAIN_SHAPES: [SceneShapeDecl; 1] = [DOMAIN_BOX];

/// A placed water source: its volume is moved, turned and sized.
pub static WATER_SOURCE_SHAPES: [SceneShapeDecl; 1] = [transform_box(true)];

/// The liquids fill an axis-aligned box, so an Initial Fill does not rotate.
pub static INITIAL_FILL_SHAPES: [SceneShapeDecl; 1] = [transform_box(false)];

/// A swarm's emitter box: any size above zero is valid.
pub static SWARM_SHAPES: [SceneShapeDecl; 1] = [transform_box(false)];

const fn point(center: ParamTriple, radius: &'static str) -> SceneShapeDecl {
    SceneShapeDecl {
        shape: SceneShape::PointRadius {
            center,
            radius: id(radius),
        },
        via: None,
        extent_limit: None,
        uniform_scale: false,
    }
}

/// A Sun, Point or Spot light: it draws no pixels, so its handle is how it
/// is clicked. Moved by its position, its reach scaled by its range.
pub static LAMP_SHAPES: [SceneShapeDecl; 1] = [point(POS, "range")];

const CENTRE: ParamTriple = [id("center_x"), id("center_y"), id("center_z")];

const fn field(axis: Option<ParamTriple>) -> SceneShapeDecl {
    let shape = SceneShape::Field {
        center: CENTRE,
        radius: id("radius"),
        falloff: id("falloff"),
        curve: id("curve"),
        axis,
        strength: Some(id("strength")),
    };
    SceneShapeDecl {
        shape,
        via: None,
        extent_limit: None,
        uniform_scale: false,
    }
}

/// A Radial force: moved by its centre, its reach scaled by its radius,
/// its falloff drawn as shells.
pub static RADIAL_FORCE_SHAPES: [SceneShapeDecl; 1] = [field(None)];

/// A Vortex: a Radial force's centre and radius, swirling round its axis.
pub static VORTEX_FORCE_SHAPES: [SceneShapeDecl; 1] =
    [field(Some([id("axis_x"), id("axis_y"), id("axis_z")]))];

/// A force along a direction (Uniform, Wind): turned by its direction.
pub static DIRECTED_FORCE_SHAPES: [SceneShapeDecl; 1] = [SceneShapeDecl {
    shape: SceneShape::Axis {
        origin: None,
        direction: [id("direction_x"), id("direction_y"), id("direction_z")],
        radius: None,
    },
    via: None,
    extent_limit: None,
    uniform_scale: false,
}];
