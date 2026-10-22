//! The shapes a scene thing declares for the viewport gizmo, by the param
//! ids that place it (PHYSICS_ENGINE_BOUNDARY_DESIGN.md section 5.2 (One
//! scene-object model)). Where a param lives on the card is the (kind,
//! property) table in `manifold_core::scene_schema`; this file holds only the
//! gizmo declarations. The ids are property names: the scene VM joins an
//! item's id onto them to find the manifest param.

use std::borrow::Cow;

use manifold_core::fluid_domain::DOMAIN_EXTENT_METRES;
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
        shape: SceneShape::Box { center: POS, rotation: if rotates { Some(ROT) } else { None }, size: SCALE },
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
    decl.extent_limit = Some(HardLimit::Between(DOMAIN_EXTENT_METRES.0, DOMAIN_EXTENT_METRES.1));
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_shape_names_the_transform_rows() {
        let ids: Vec<&str> = ITEM_SHAPES[0].shape.param_ids().collect();
        assert_eq!(ids.len(), 9);
        assert!(ids.contains(&"pos_x") && ids.contains(&"rot_z") && ids.contains(&"scale_y"));
    }
}
