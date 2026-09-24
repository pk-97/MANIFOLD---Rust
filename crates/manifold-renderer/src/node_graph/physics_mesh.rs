//! Asset preparation for standard Box3D hulls. Runs once on the source loader,
//! never during simulation. Rendering and collision use the same source selection.
use std::path::Path;

use super::decode_cache::cached_load_gltf_mesh;
use super::effect_node::EffectNodeContext;
use super::gltf_load::{DEFAULT_MATERIAL_MESH_PARAM, GltfMeshSelector};
use super::mesh_partition::{Fragment, partition};
use super::parameters::ParamValue;
use super::physics::ColliderGeometry;
use super::primitives::gltf_mesh_source::{apply_mesh_fit, apply_translate};
use crate::generators::mesh_common::MeshVertex;

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
        let selector = if self.material == DEFAULT_MATERIAL_MESH_PARAM {
            GltfMeshSelector::DefaultMaterial
        } else if self.material >= 0 {
            GltfMeshSelector::Material {
                material_index: self.material as u32,
            }
        } else if self.mesh < 0 {
            GltfMeshSelector::WholeScene
        } else if self.primitive < 0 {
            GltfMeshSelector::Mesh {
                mesh_index: self.mesh as u32,
            }
        } else {
            GltfMeshSelector::Primitive {
                mesh_index: self.mesh as u32,
                primitive_index: self.primitive as u32,
            }
        };
        let vertices = cached_load_gltf_mesh(path, selector)?;
        let vertices = apply_translate(
            apply_mesh_fit(vertices, self.fit, self.recenter),
            self.translate,
        );
        select_fragment(vertices, self.fragment_count, self.fragment_index)
    }
}

fn fragments(vertices: &[MeshVertex], count: usize) -> Result<Vec<Fragment>, String> {
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

/// Spatially fitted convex parts. This is approximate collision geometry; the
/// scan itself is unchanged. Thin/open scan surfaces get a small explicit shell
/// (0.1% of the selected mesh's longest dimension) so native solid hulls have volume.
pub fn prepare_colliders(vertices: &[MeshVertex], count: u32) -> Result<ColliderGeometry, String> {
    let count = (count as usize).min(vertices.len() / 3);
    let pieces = fragments(vertices, count)?;
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    for vertex in vertices {
        for axis in 0..3 {
            min[axis] = min[axis].min(vertex.position[axis]);
            max[axis] = max[axis].max(vertex.position[axis]);
        }
    }
    let thickness = (0..3).map(|i| max[i] - min[i]).fold(0.0_f32, f32::max) * 0.001;
    if !thickness.is_finite() || thickness <= 0.0 {
        return Err("Physics mesh has no usable extent".into());
    }
    let hulls = pieces
        .into_iter()
        .map(|piece| {
            let mut points = Vec::with_capacity(piece.triangle_ids.len() * 6);
            for triangle in piece.triangle_ids {
                let tri = &vertices[triangle * 3..triangle * 3 + 3];
                let a = tri[0].position;
                let b: [f32; 3] = std::array::from_fn(|i| tri[1].position[i] - a[i]);
                let c: [f32; 3] = std::array::from_fn(|i| tri[2].position[i] - a[i]);
                let normal = [
                    b[1] * c[2] - b[2] * c[1],
                    b[2] * c[0] - b[0] * c[2],
                    b[0] * c[1] - b[1] * c[0],
                ];
                let length = normal.iter().map(|v| v * v).sum::<f32>().sqrt();
                let shell = normal.map(|v| {
                    if length > 0.0 {
                        v / length * thickness * 0.5
                    } else {
                        0.0
                    }
                });
                for vertex in tri {
                    points.push(std::array::from_fn(|i| vertex.position[i] + shell[i]));
                    points.push(std::array::from_fn(|i| vertex.position[i] - shell[i]));
                }
            }
            points.sort_unstable_by(|a, b| {
                a[0].total_cmp(&b[0])
                    .then(a[1].total_cmp(&b[1]))
                    .then(a[2].total_cmp(&b[2]))
            });
            points.dedup();
            manifold_physics::cook_hull(&points).map_err(|e| e.to_string())
        })
        .collect::<Result<Vec<_>, String>>()?;
    Ok(ColliderGeometry { hulls })
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytemuck::Zeroable;
    #[test]
    fn split_preserves_every_triangle_and_attributes_once() {
        let vertices: Vec<_> = (0..12)
            .flat_map(|i| {
                [
                    [i as f32, 0.0, 0.0],
                    [i as f32, 1.0, 0.0],
                    [i as f32, 0.0, 1.0],
                ]
                .map(|position| MeshVertex {
                    position,
                    normal: [1.0, 0.0, 0.0],
                    uv: [i as f32, 0.0],
                    ..MeshVertex::zeroed()
                })
            })
            .collect();
        let mut found = Vec::new();
        for piece in 0..4 {
            let selected = select_fragment(vertices.clone(), 4, piece).unwrap();
            for tri in selected.chunks_exact(3) {
                let id = tri[0].uv[0] as usize;
                assert_eq!(
                    bytemuck::cast_slice::<_, u8>(tri),
                    bytemuck::cast_slice::<_, u8>(&vertices[id * 3..id * 3 + 3])
                );
                found.push(id);
            }
        }
        found.sort_unstable();
        assert_eq!(found, (0..12).collect::<Vec<_>>());
        let colliders = prepare_colliders(&vertices, 4).unwrap();
        assert_eq!(colliders.hulls.len(), 4);
        assert!(colliders.hulls.iter().all(|h| h.len() >= 4));
    }
}
