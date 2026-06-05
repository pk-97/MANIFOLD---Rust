//! Semantic preview encoding — how a node's output texture is shown in the
//! editor's node-output preview.
//!
//! One tone curve can't serve every output: a colour image wants raw, a
//! density wants its blacks kept black and lifted, a force field wants its
//! direction and magnitude shown — the same way Unreal's buffer visualization
//! decodes normals, depth and velocity each their own way rather than with a
//! single brightness slider.
//!
//! The signal isn't in the pixels (nearly every graph texture is allocated
//! `rgba16float` regardless of what it carries), it's in the node: its
//! [`descriptor`](crate::node_graph::descriptor) `category` / `role`, and the
//! name of the output port being previewed (especially a group's interface
//! name like `forceField`). [`PreviewEncoding::derive`] folds those into one of
//! three Phase-1 encodings; anything unclassified falls to the safe
//! [`PreviewEncoding::ScalarLift`], which keeps blacks black and lifts the
//! dark — strictly better than the old grey-centred curve.

use crate::node_graph::descriptor::{Category, Role, descriptor_for};

/// How the node-output preview should render a captured texture. Phase 1: three
/// encodings; normal-map decode and depth colormaps are a later pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PreviewEncoding {
    /// Show the texture as-is. Colour images, composites, final outputs —
    /// already meant to be looked at.
    #[default]
    Color,
    /// Unsigned scalar / data field: 0 stays black, dark values lifted by a
    /// fixed asinh curve. Density, masks, depth, and the safe default for
    /// anything not yet classified.
    ScalarLift,
    /// 2D vector field: direction → hue, magnitude → brightness (the standard
    /// optical-flow colour wheel). Force fields, flow, gradients,
    /// displacement, UV / coordinate maps.
    VectorField,
}

impl PreviewEncoding {
    /// Derive the encoding for a previewed output from its node `type_id` and
    /// the name of the port being shown — for a group that's the interface
    /// output name (`forceField`), otherwise the producing node's output port.
    ///
    /// Priority: a semantically-loaded port name wins (it's the most specific —
    /// a blur node feeding a group's `forceField` output is still a force
    /// field); then the node's own `type_id` keywords; then its descriptor
    /// category / role; then the safe `ScalarLift` default.
    pub fn derive(type_id: &str, port_name: &str) -> Self {
        let port = port_name.to_ascii_lowercase();
        if name_is_vector(&port) {
            return Self::VectorField;
        }
        if name_is_scalar(&port) {
            return Self::ScalarLift;
        }
        if name_is_color(&port) {
            return Self::Color;
        }

        let tid = type_id.to_ascii_lowercase();
        if name_is_vector(&tid) {
            return Self::VectorField;
        }
        if name_is_scalar(&tid) {
            return Self::ScalarLift;
        }

        if let Some(d) = descriptor_for(type_id) {
            match d.category {
                Category::FieldsAndCoordinates => return Self::VectorField,
                Category::Mask => return Self::ScalarLift,
                Category::DetectionAndSampling => return Self::ScalarLift,
                Category::ColorAndTone
                | Category::Composite
                | Category::Generate
                | Category::Stylize
                | Category::DistortAndWarp
                | Category::BlurAndSharpen
                | Category::MaterialsAndLighting
                | Category::Geometry3D => return Self::Color,
                _ => {}
            }
            if d.role == Role::Sink {
                return Self::Color;
            }
        }

        // Unclassified (Noise / Math / Routing / Control / Particles /
        // Uncategorized): keep blacks black and lift. Safer than `Color`,
        // whose failure mode on a dark field is the invisible-black we're
        // fixing.
        Self::ScalarLift
    }
}

fn name_is_vector(s: &str) -> bool {
    [
        "force", "flow", "velocity", "gradient", "grad", "vector", "displace", "curl", "vortic",
        "uv", "coord",
    ]
    .iter()
    .any(|k| s.contains(k))
}

fn name_is_scalar(s: &str) -> bool {
    [
        "density", "mask", "depth", "height", "lumin", "occlus", "falloff", "sdf", "distance",
    ]
    .iter()
    .any(|k| s.contains(k))
}

fn name_is_color(s: &str) -> bool {
    [
        "color", "colour", "albedo", "image", "composite", "display", "final",
    ]
    .iter()
    .any(|k| s.contains(k))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn group_force_field_port_picks_vector() {
        // The headline case: a group's `forceField` output, even though the
        // producer behind it is a blur node.
        assert_eq!(
            PreviewEncoding::derive("node.gaussian_blur", "forceField"),
            PreviewEncoding::VectorField
        );
    }

    #[test]
    fn field_node_descriptor_picks_vector() {
        // gradient_central_diff / rotate_vec2_by_angle are FieldsAndCoordinates.
        assert_eq!(
            PreviewEncoding::derive("node.gradient_central_diff", "out"),
            PreviewEncoding::VectorField
        );
        assert_eq!(
            PreviewEncoding::derive("node.rotate_vec2_by_angle", "out"),
            PreviewEncoding::VectorField
        );
    }

    #[test]
    fn density_port_picks_scalar() {
        assert_eq!(
            PreviewEncoding::derive("node.gaussian_blur", "blurredDensity"),
            PreviewEncoding::ScalarLift
        );
    }

    #[test]
    fn blur_of_unknown_is_color_but_scalar_default_for_unclassified() {
        // A plain blur with a generic port → Color (its descriptor).
        assert_eq!(
            PreviewEncoding::derive("node.gaussian_blur", "out"),
            PreviewEncoding::Color
        );
        // A type with no descriptor and a generic port → safe ScalarLift.
        assert_eq!(
            PreviewEncoding::derive("node.totally_unknown_xyz", "out"),
            PreviewEncoding::ScalarLift
        );
    }
}
