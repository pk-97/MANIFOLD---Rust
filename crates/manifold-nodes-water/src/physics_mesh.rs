//! Native convex collider preparation for imported mesh geometry.

use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::scene::physics_mesh::fragments;
use crate::physics::ColliderGeometry;

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
    use super::prepare_colliders;
    use manifold_node_engine::mesh::MeshVertex;
    use manifold_node_engine::scene::physics_mesh::select_fragment;
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
