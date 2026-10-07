//! CPU geometry preparation for `node.fluid_role_source`.
//!
//! The source node deliberately publishes only immutable, local-space
//! `TriangleMesh` values.  Simulation state, native handles, and live object
//! transforms belong to the fluid world consumer.

use std::path::Path;

use ahash::AHashMap;
use manifold_physics::TriangleMesh;

use super::{CompoundPreparation, WiredPreparation};
use crate::mesh::MeshVertex;
use crate::platonic::{platonic_mesh, platonic_points};
use crate::scene::physics_mesh::{MeshSelection, load_compound_materials, prepare_colliders, transform_vertices};
use crate::scene::transform::Transform;

/// The two preparation modes exposed by the source node.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GeometryMode {
    CollisionProxy,
    ClosedMesh,
}

/// Prepare one or more immutable fluid meshes from an imported source or a
/// built-in Platonic solid.
pub fn prepare_geometry(
    path: &Path,
    selection: MeshSelection,
    shape: u32,
    radius: f32,
    source_transform: Transform,
    mode: GeometryMode,
    compound: Option<&CompoundPreparation>,
) -> Result<Vec<TriangleMesh>, String> {
    let label = if path.as_os_str().is_empty() {
        format!("builtin shape {shape}")
    } else {
        path.display().to_string()
    };
    if compound.is_some() && path.as_os_str().is_empty() {
        return Err("compound material selection requires an imported mesh path".into());
    }
    let meshes = match mode {
        GeometryMode::CollisionProxy => {
            if path.as_os_str().is_empty() {
                let points: Vec<_> = platonic_points(shape)
                    .iter()
                    .map(|point| [point[0] * radius, point[1] * radius, point[2] * radius])
                    .collect();
                vec![
                    manifold_physics::cook_hull_mesh(&transform_points(&points, source_transform)?)
                        .map_err(|error| error.to_string())?,
                ]
            } else {
                let mut vertices = if let Some(compound) = compound {
                    load_compound_materials(
                        path,
                        selection,
                        compound.materials,
                        compound.part_transforms,
                    )?
                } else {
                    selection.load(path)?
                };
                transform_vertices(&mut vertices, source_transform)?;
                let hulls = prepare_colliders(&vertices, selection.collider_parts)
                    .map_err(|error| error.to_string())?
                    .hulls;
                hulls
                    .into_iter()
                    .map(|points| {
                        manifold_physics::cook_hull_mesh(&points).map_err(|error| error.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?
            }
        }
        GeometryMode::ClosedMesh => {
            let vertices = if path.as_os_str().is_empty() {
                let mut vertices = platonic_mesh(shape).to_vec();
                for vertex in &mut vertices {
                    for axis in 0..3 {
                        vertex.position[axis] *= radius;
                    }
                }
                transform_vertices(&mut vertices, source_transform)?;
                vertices
            } else {
                let mut vertices = if let Some(compound) = compound {
                    load_compound_materials(
                        path,
                        selection,
                        compound.materials,
                        compound.part_transforms,
                    )?
                } else {
                    selection.load(path)?
                };
                transform_vertices(&mut vertices, source_transform)?;
                vertices
            };
            vec![weld_triangle_list(&vertices)?]
        }
    };

    if mode == GeometryMode::ClosedMesh {
        for mesh in &meshes {
            manifold_fluids::validate_mesh(mesh).map_err(|error| {
                format!("{label}: {error}; select Collision Proxy for an approximate closed hull")
            })?;
        }
    }
    if meshes.is_empty() {
        return Err(format!("{label}: preparation produced no geometry"));
    }
    Ok(meshes)
}

/// Load every connected visible source independently, applying the same
/// selectors, fit and fragment operations as rendering before part transforms.
pub fn prepare_wired_geometry(
    wired: &WiredPreparation,
    source_transform: Transform,
    mode: GeometryMode,
    collider_parts: u32,
) -> Result<Vec<TriangleMesh>, String> {
    let mut vertices = Vec::new();
    for (slot, source) in wired.sources.iter().enumerate() {
        let Some(source) = source else { continue };
        let mut part = source
            .load_vertices()
            .map_err(|error| format!("mesh part {slot}: {error}"))?;
        transform_vertices(&mut part, wired.part_transforms[slot])?;
        vertices.extend(part);
    }
    transform_vertices(&mut vertices, source_transform)?;
    match mode {
        GeometryMode::CollisionProxy => prepare_colliders(&vertices, collider_parts)
            .map_err(|error| error.to_string())?
            .hulls
            .iter()
            .map(|points| {
                manifold_physics::cook_hull_mesh(points).map_err(|error| error.to_string())
            })
            .collect(),
        GeometryMode::ClosedMesh => {
            let mesh = weld_triangle_list(&vertices)?;
            manifold_fluids::validate_mesh(&mesh).map_err(|error| {
                format!("{error}; select Collision Proxy for an approximate closed hull")
            })?;
            Ok(vec![mesh])
        }
    }
}

fn transform_points(points: &[[f32; 3]], transform: Transform) -> Result<Vec<[f32; 3]>, String> {
    let mut vertices = points
        .iter()
        .copied()
        .map(|position| MeshVertex {
            position,
            ..bytemuck::Zeroable::zeroed()
        })
        .collect::<Vec<_>>();
    transform_vertices(&mut vertices, transform)?;
    Ok(vertices.into_iter().map(|vertex| vertex.position).collect())
}

/// Convert an unindexed triangle list to an indexed mesh by welding only
/// exactly equal coordinates.  Signed zero is canonicalized because it is the
/// same geometric coordinate; no tolerance-based simplification is performed.
pub(crate) fn weld_triangle_list(vertices: &[MeshVertex]) -> Result<TriangleMesh, String> {
    if vertices.is_empty() || !vertices.len().is_multiple_of(3) {
        return Err("closed mesh source must contain a nonempty triangle list".into());
    }
    let mut indices = AHashMap::<[u32; 3], u32>::new();
    let mut positions = Vec::new();
    let mut triangles = Vec::with_capacity(vertices.len() / 3);
    for triangle in vertices.chunks_exact(3) {
        let mut indexed = [0_u32; 3];
        for (slot, vertex) in triangle.iter().enumerate() {
            let key = vertex.position.map(canonical_bits);
            let index = match indices.get(&key) {
                Some(&index) => index,
                None => {
                    let index = u32::try_from(positions.len())
                        .map_err(|_| "closed mesh has too many unique vertices".to_string())?;
                    indices.insert(key, index);
                    positions.push(vertex.position);
                    index
                }
            };
            indexed[slot] = index;
        }
        triangles.push(indexed);
    }
    Ok(TriangleMesh {
        vertices: positions,
        triangles,
    })
}

#[inline]
fn canonical_bits(value: f32) -> u32 {
    let bits = value.to_bits();
    if bits == (-0.0_f32).to_bits() {
        0
    } else {
        bits
    }
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::testkit::fluid_role_source::cube_triangle_list;

    #[test]
    fn scene_physics_fluid_source_welds_attribute_seams_without_position_changes() {
        let mut vertices = cube_triangle_list();
        vertices[1].normal = [1.0, 0.0, 0.0];
        vertices[2].normal = [0.0, 1.0, 0.0];
        let mesh = weld_triangle_list(&vertices).unwrap();
        assert_eq!(mesh.vertices.len(), 8);
        assert_eq!(mesh.triangles.len(), 12);
        let reconstructed: Vec<_> = mesh
            .triangles
            .iter()
            .flatten()
            .map(|&index| mesh.vertices[index as usize])
            .collect();
        assert_eq!(
            reconstructed,
            vertices
                .iter()
                .map(|vertex| vertex.position)
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn scene_physics_fluid_source_canonicalizes_signed_zero_exactly() {
        let vertices = vec![
            MeshVertex {
                position: [-0.0, 0.0, 0.0],
                ..bytemuck::Zeroable::zeroed()
            },
            MeshVertex {
                position: [1.0, 0.0, 0.0],
                ..bytemuck::Zeroable::zeroed()
            },
            MeshVertex {
                position: [0.0, 1.0, 0.0],
                ..bytemuck::Zeroable::zeroed()
            },
            MeshVertex {
                position: [0.0, 0.0, 0.0],
                ..bytemuck::Zeroable::zeroed()
            },
            MeshVertex {
                position: [1.0, 0.0, 0.0],
                ..bytemuck::Zeroable::zeroed()
            },
            MeshVertex {
                position: [0.0, 1.0, 0.0],
                ..bytemuck::Zeroable::zeroed()
            },
        ];
        let mesh = weld_triangle_list(&vertices).unwrap();
        assert_eq!(mesh.vertices.len(), 3);
        assert_eq!(mesh.triangles, vec![[0, 1, 2], [0, 1, 2]]);
    }

    #[test]
    fn scene_physics_fluid_source_proxy_and_builtin_closed_meshes_are_closed() {
        let selection = MeshSelection {
            mesh: -1,
            primitive: -1,
            material: -1,
            fit: false,
            recenter: true,
            translate: [0.0; 3],
            fragment_count: 1,
            fragment_index: 0,
            collider_parts: 1,
        };
        let proxy = prepare_geometry(
            Path::new(""),
            selection,
            1,
            1.0,
            Transform::default(),
            GeometryMode::CollisionProxy,
            None,
        )
        .unwrap();
        assert!(!proxy.is_empty());
        for mesh in &proxy {
            manifold_fluids::validate_mesh(mesh).unwrap();
        }
        let closed = prepare_geometry(
            Path::new(""),
            selection,
            1,
            1.0,
            Transform::default(),
            GeometryMode::ClosedMesh,
            None,
        )
        .unwrap();
        assert_eq!(closed[0].vertices.len(), 8);
        manifold_fluids::validate_mesh(&closed[0]).unwrap();
    }

}
