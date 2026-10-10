//! Shared mesh selection and authored transforms for imported scene geometry.
use std::path::Path;

use crate::exec::effect_node::EffectNodeContext;
use super::mesh_partition::{Fragment, partition};
use crate::parameters::ParamValue;
use crate::mesh::MeshVertex;
use crate::scene::transform::Transform;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MeshSelection {
    pub mesh: i32,
    pub primitive: i32,
    pub material: i32,
    pub fit: bool,
    pub recenter: bool,
    pub translate: [f32; 3],
    pub fragment_count: u32,
    pub fragment_index: u32,
    pub collider_parts: u32,
}

impl MeshSelection {
    pub fn with_material(mut self, material: i32) -> Self {
        self.material = material;
        self
    }

    pub fn from_context(ctx: &EffectNodeContext<'_, '_>) -> Self {
        Self {
            mesh: ctx.param_f32("mesh_index", -1.0).round() as i32,
            primitive: ctx.param_f32("primitive_index", -1.0).round() as i32,
            material: ctx.param_f32("material_index", -1.0).round() as i32,
            fit: matches!(ctx.params.get("fit"), Some(ParamValue::Enum(1)))
                || ctx.param_f32("fit", 0.0) == 1.0,
            recenter: !matches!(ctx.params.get("recenter"), Some(ParamValue::Bool(false))),
            translate: ["translate_x", "translate_y", "translate_z"].map(|p| ctx.param_f32(p, 0.0)),
            fragment_count: ctx
                .param_f32("fragment_count", 1.0)
                .round()
                .clamp(1.0, 64.0) as u32,
            fragment_index: ctx.param_f32("fragment_index", 0.0).round().max(0.0) as u32,
            collider_parts: ctx
                .param_f32("collider_parts", 32.0)
                .round()
                .clamp(1.0, 64.0) as u32,
        }
    }

    pub fn load(&self, path: &Path) -> Result<Vec<MeshVertex>, String> {
        let vertices = super::mesh_asset_source::load_mesh(path, self)?;
        select_fragment(vertices, self.fragment_count, self.fragment_index)
    }
}

/// Apply a fixed authored transform to source vertices before native cooking.
/// Billboard transforms are camera-relative and therefore invalid for static
/// collider geometry.
pub fn transform_vertices(vertices: &mut [MeshVertex], transform: Transform) -> Result<(), String> {
    validate_transform(transform)?;
    let (cx, sx) = (transform.rot_euler[0].cos(), transform.rot_euler[0].sin());
    let (cy, sy) = (transform.rot_euler[1].cos(), transform.rot_euler[1].sin());
    let (cz, sz) = (transform.rot_euler[2].cos(), transform.rot_euler[2].sin());
    // Column-major Rz * Ry * Rx, matching render_scene's model_matrix.
    let r = [
        [cz * cy, sz * cy, -sy],
        [cz * sy * sx - sz * cx, sz * sy * sx + cz * cx, cy * sx],
        [cz * sy * cx + sz * sx, sz * sy * cx - cz * sx, cy * cx],
    ];
    for vertex in vertices {
        let p = vertex.position;
        let scaled = [
            p[0] * transform.scale[0],
            p[1] * transform.scale[1],
            p[2] * transform.scale[2],
        ];
        vertex.position = [
            transform.pos[0] + r[0][0] * scaled[0] + r[1][0] * scaled[1] + r[2][0] * scaled[2],
            transform.pos[1] + r[0][1] * scaled[0] + r[1][1] * scaled[1] + r[2][1] * scaled[2],
            transform.pos[2] + r[0][2] * scaled[0] + r[1][2] * scaled[1] + r[2][2] * scaled[2],
        ];
    }
    Ok(())
}

pub fn validate_transform(transform: Transform) -> Result<(), String> {
    if transform.billboard {
        return Err("Physics source and compound part transforms cannot use billboard mode".into());
    }
    let finite = transform
        .pos
        .into_iter()
        .chain(transform.rot_euler)
        .chain(transform.scale)
        .all(f32::is_finite);
    if !finite {
        return Err("Physics source and compound part transforms must be finite".into());
    }
    Ok(())
}

/// Partition a triangle mesh into spatial fragments for scene selection.
pub fn fragments(vertices: &[MeshVertex], count: usize) -> Result<Vec<Fragment>, String> {
    if vertices.is_empty() || !vertices.len().is_multiple_of(3) {
        return Err("Physics needs a nonempty triangle mesh".into());
    }
    let points: Vec<_> = vertices.iter().map(|v| v.position).collect();
    let triangles: Vec<_> = (0..vertices.len() as u32)
        .step_by(3)
        .map(|i| [i, i + 1, i + 2])
        .collect();
    partition(&points, &triangles, count).map_err(|e| e.to_string())
}

/// Original triangle attributes and winding are copied unchanged. Fitting happens
/// before selection, so every part remains in the original object's coordinates.
pub fn select_fragment(
    vertices: Vec<MeshVertex>,
    count: u32,
    index: u32,
) -> Result<Vec<MeshVertex>, String> {
    if count == 0 || index >= count {
        return Err("Piece index must be smaller than the piece count".into());
    }
    if count == 1 {
        return Ok(vertices);
    }
    let pieces = fragments(&vertices, count as usize)?;
    let piece = &pieces[index as usize];
    let mut selected = Vec::with_capacity(piece.triangle_ids.len() * 3);
    for &triangle in &piece.triangle_ids {
        selected.extend_from_slice(&vertices[triangle * 3..triangle * 3 + 3]);
    }
    Ok(selected)
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;

    #[test]
    fn transform_vertices_applies_trs_before_cooking() {
        let mut vertices = vec![MeshVertex {
            position: [1.0, 0.0, 0.0],
            ..MeshVertex::zeroed()
        }];
        transform_vertices(
            &mut vertices,
            Transform {
                pos: [2.0, 3.0, 4.0],
                rot_euler: [0.0, 0.0, std::f32::consts::FRAC_PI_2],
                scale: [2.0, 1.0, 1.0],
                billboard: false,
            },
        )
        .unwrap();
        assert!((vertices[0].position[0] - 2.0).abs() < 1e-6);
        assert!((vertices[0].position[1] - 5.0).abs() < 1e-6);
        assert!((vertices[0].position[2] - 4.0).abs() < 1e-6);
    }

    #[test]
    fn invalid_collider_transform_is_rejected() {
        assert!(validate_transform(Transform {
            scale: [f32::NAN, 1.0, 1.0],
            ..Transform::default()
        })
        .is_err());
        assert!(validate_transform(Transform {
            billboard: true,
            ..Transform::default()
        })
        .is_err());
    }

}
