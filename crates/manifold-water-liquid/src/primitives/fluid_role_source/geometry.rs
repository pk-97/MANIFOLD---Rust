//! CPU geometry preparation for `node.fluid_role_source`.
//!
//! The source node deliberately publishes only immutable, local-space
//! `TriangleMesh` values: the exact welded surface, closed parts solid and
//! open parts shells (the water's mesh tree decides which at query time).
//! Simulation state, native handles, and live object transforms belong to the
//! fluid world consumer.

use std::path::Path;

use ahash::AHashMap;
use manifold_physics::TriangleMesh;

use super::{CompoundPreparation, WiredPreparation};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::scene::mesh_source::MeshSource;
use manifold_node_engine::scene::mesh_selection::MeshSelection;
use manifold_water_rigid::physics_mesh::transform_vertices;
use manifold_water_rigid::physics_mesh::load_compound_materials;
use manifold_node_engine::scene::transform::Transform;

/// What a role with no mesh fills: a unit cube, so its transform's scale is
/// its size.
const BUILTIN_CUBE: MeshSource = MeshSource::Cube { size: 1.0 };

manifold_core::testkit_visible! {
/// Prepare one immutable fluid mesh from an imported source, or the unit cube
/// when there is no file.
pub(crate) fn prepare_geometry(
    path: &Path,
    selection: MeshSelection,
    source_transform: Transform,
    compound: Option<&CompoundPreparation>,
) -> Result<Vec<TriangleMesh>, String> {
    let label = if path.as_os_str().is_empty() { "builtin cube".to_string() } else { path.display().to_string() };
    if compound.is_some() && path.as_os_str().is_empty() {
        return Err("compound material selection requires an imported mesh path".into());
    }
    let mut vertices = if path.as_os_str().is_empty() {
        BUILTIN_CUBE.load_vertices()?
    } else if let Some(compound) = compound {
        load_compound_materials(path, selection, compound.materials, compound.part_transforms)?
    } else {
        selection.load(path)?
    };
    transform_vertices(&mut vertices, source_transform)?;
    let mesh = weld_triangle_list(&vertices)?;
    manifold_physics::closed_triangles(&mesh).map_err(|error| format!("{label}: {error}"))?;
    Ok(vec![mesh])
}
}

manifold_core::testkit_visible! {
/// Load every connected visible source independently, applying the same
/// selectors, fit and fragment operations as rendering before part transforms.
pub(crate) fn prepare_wired_geometry(
    wired: &WiredPreparation,
    source_transform: Transform,
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
    let mesh = weld_triangle_list(&vertices)?;
    manifold_physics::closed_triangles(&mesh).map_err(|error| error.to_string())?;
    Ok(vec![mesh])
}
}

/// Convert an unindexed triangle list to an indexed mesh by welding only
/// exactly equal coordinates.  Signed zero is canonicalized because it is the
/// same geometric coordinate; no tolerance-based simplification is performed.
pub(crate) fn weld_triangle_list(vertices: &[MeshVertex]) -> Result<TriangleMesh, String> {
    if vertices.is_empty() || !vertices.len().is_multiple_of(3) {
        return Err("mesh source must contain a nonempty triangle list".into());
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
                        .map_err(|_| "mesh has too many unique vertices".to_string())?;
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
    fn scene_physics_fluid_source_builtin_cube_is_closed() {
        let selection = super::super::default_selection();
        let closed = prepare_geometry(Path::new(""), selection, Transform::default(), None).unwrap();
        assert_eq!(closed[0].vertices.len(), 8);
        manifold_fluids::validate_mesh(&closed[0]).unwrap();
    }

}
