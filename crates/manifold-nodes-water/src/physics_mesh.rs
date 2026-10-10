//! Native convex collider preparation for imported mesh geometry.

use std::path::Path;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::scene::physics_mesh::{MeshSelection, fragments, transform_vertices};
use manifold_node_engine::scene::transform::Transform;
use crate::physics::ColliderGeometry;

pub(crate) const PART_PORTS: [&str; 64] = [
    "part_0", "part_1", "part_2", "part_3", "part_4", "part_5", "part_6", "part_7", "part_8",
    "part_9", "part_10", "part_11", "part_12", "part_13", "part_14", "part_15", "part_16",
    "part_17", "part_18", "part_19", "part_20", "part_21", "part_22", "part_23", "part_24",
    "part_25", "part_26", "part_27", "part_28", "part_29", "part_30", "part_31", "part_32",
    "part_33", "part_34", "part_35", "part_36", "part_37", "part_38", "part_39", "part_40",
    "part_41", "part_42", "part_43", "part_44", "part_45", "part_46", "part_47", "part_48",
    "part_49", "part_50", "part_51", "part_52", "part_53", "part_54", "part_55", "part_56",
    "part_57", "part_58", "part_59", "part_60", "part_61", "part_62", "part_63",
];

pub(crate) fn parse_compound_materials(
    ctx: &EffectNodeContext<'_, '_>,
) -> Result<([Option<i32>; 64], bool), String> {
    let Some(table) = ctx
        .params
        .get("compound_materials")
        .and_then(ParamValue::as_table)
    else {
        return Ok(([None; 64], false));
    };
    if table.col_count() != 2 {
        return Err("compound_materials must have rows shaped [slot, material_index]".into());
    }
    let mut materials = [None; 64];
    for row in table.rows() {
        let slot = row[0];
        let material = row[1];
        if !slot.is_finite() || slot.fract() != 0.0 || !(0.0..64.0).contains(&slot) {
            return Err("compound_materials contains a slot outside 0..63".into());
        }
        if !material.is_finite()
            || material.fract() != 0.0
            || !((i32::MIN as f32)..=(i32::MAX as f32)).contains(&material)
        {
            return Err("compound_materials contains an invalid material index".into());
        }
        let slot = slot as usize;
        if materials[slot].is_some() {
            return Err(format!("compound_materials contains duplicate slot {slot}"));
        }
        materials[slot] = Some(material as i32);
    }
    Ok((materials, true))
}

pub(crate) fn load_compound_materials(
    path: &Path,
    selection: MeshSelection,
    materials: [Option<i32>; 64],
    part_transforms: [Transform; 64],
) -> Result<Vec<MeshVertex>, String> {
    let mut vertices = Vec::new();
    for slot in 0..64 {
        let Some(material) = materials[slot] else {
            continue;
        };
        let mut part = selection.with_material(material).load(path)?;
        transform_vertices(&mut part, part_transforms[slot])?;
        vertices.extend(part);
    }
    if vertices.is_empty() {
        return Err("compound material selection produced no geometry".into());
    }
    Ok(vertices)
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
