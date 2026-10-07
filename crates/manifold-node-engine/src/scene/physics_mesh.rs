//! Asset preparation for standard Box3D hulls. Runs once on the source loader,
//! never during simulation. Rendering and collision use the same source selection.
use std::path::Path;

use crate::exec::effect_node::EffectNodeContext;
use super::mesh_partition::{Fragment, partition};
use crate::parameters::ParamValue;
use crate::water::physics::ColliderGeometry;
use crate::mesh::MeshVertex;
use crate::scene::transform::Transform;

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
