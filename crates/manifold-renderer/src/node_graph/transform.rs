//! `Transform` — port-data type carried on
//! [`PortType::Transform`](crate::node_graph::ports::PortType::Transform) wires.
//!
//! Local TRS of one scene object. CPU-only wire value, composed to a model
//! matrix by the consuming renderer per frame. Euler radians, XYZ application
//! order — matching `render_scene`'s existing `model_matrix`
//! (`render_scene.rs:419`), which is unchanged by this port's introduction.
//!
//! Produced by `node.transform_3d`, consumed (P2 of
//! `docs/SCENE_BUILD_AND_GROUP_PARAMS_DESIGN.md`) by `render_scene`'s
//! `transform_n` ports instead of nine per-object params. Same CPU-struct
//! lifetime model as [`Camera`](crate::node_graph::camera::Camera),
//! [`Light`](crate::node_graph::light::Light), and
//! [`Material`](crate::node_graph::material::Material) — no GPU resource on
//! the wire, so zero interaction with texture prebinding or pooling.

/// Local TRS of one scene object. CPU-only wire value (`PortType::Transform`),
/// composed to a model matrix by the consuming renderer per frame. Euler
/// radians, XYZ application order — matching `render_scene`'s existing
/// `model_matrix` (`render_scene.rs:419`), which is unchanged.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transform {
    pub pos: [f32; 3],
    pub rot_euler: [f32; 3], // radians
    pub scale: [f32; 3],
}

impl Default for Transform {
    fn default() -> Self {
        Self {
            pos: [0.0; 3],
            rot_euler: [0.0; 3],
            scale: [1.0; 3],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_identity_trs() {
        let t = Transform::default();
        assert_eq!(t.pos, [0.0, 0.0, 0.0]);
        assert_eq!(t.rot_euler, [0.0, 0.0, 0.0]);
        assert_eq!(t.scale, [1.0, 1.0, 1.0]);
    }

    #[test]
    fn transform_is_copy_and_cheap_to_clone() {
        let t = Transform::default();
        let _copy = t;
        let _another = t;
    }
}
