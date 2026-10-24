//! Loading and placing a body's source geometry before it becomes a
//! collision shape (`manifold_physics::collision_shape`).

use std::borrow::Cow;
use std::path::Path;

use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::ParamValue;
use manifold_node_engine::ports::{NodeInput, PortKind, PortType};
use manifold_node_engine::scene::mesh_selection::MeshSelection;
use manifold_node_engine::scene::transform::Transform;

/// A fluid role source's fixed part inputs (`part_k`); a scene Water
/// Source wires only `mesh_0`. A rigid body's parts grow ([`PartPorts`]).
pub const PART_PORTS: [&str; 64] = [
    "part_0", "part_1", "part_2", "part_3", "part_4", "part_5", "part_6", "part_7", "part_8",
    "part_9", "part_10", "part_11", "part_12", "part_13", "part_14", "part_15", "part_16",
    "part_17", "part_18", "part_19", "part_20", "part_21", "part_22", "part_23", "part_24",
    "part_25", "part_26", "part_27", "part_28", "part_29", "part_30", "part_31", "part_32",
    "part_33", "part_34", "part_35", "part_36", "part_37", "part_38", "part_39", "part_40",
    "part_41", "part_42", "part_43", "part_44", "part_45", "part_46", "part_47", "part_48",
    "part_49", "part_50", "part_51", "part_52", "part_53", "part_54", "part_55", "part_56",
    "part_57", "part_58", "part_59", "part_60", "part_61", "part_62", "part_63",
];

/// A fluid role source's `mesh_k` inputs, paired with `part_k`.
pub const MESH_PORTS: [&str; 64] = [
    "mesh_0", "mesh_1", "mesh_2", "mesh_3", "mesh_4", "mesh_5", "mesh_6", "mesh_7", "mesh_8",
    "mesh_9", "mesh_10", "mesh_11", "mesh_12", "mesh_13", "mesh_14", "mesh_15", "mesh_16",
    "mesh_17", "mesh_18", "mesh_19", "mesh_20", "mesh_21", "mesh_22", "mesh_23", "mesh_24",
    "mesh_25", "mesh_26", "mesh_27", "mesh_28", "mesh_29", "mesh_30", "mesh_31", "mesh_32",
    "mesh_33", "mesh_34", "mesh_35", "mesh_36", "mesh_37", "mesh_38", "mesh_39", "mesh_40",
    "mesh_41", "mesh_42", "mesh_43", "mesh_44", "mesh_45", "mesh_46", "mesh_47", "mesh_48",
    "mesh_49", "mesh_50", "mesh_51", "mesh_52", "mesh_53", "mesh_54", "mesh_55", "mesh_56",
    "mesh_57", "mesh_58", "mesh_59", "mesh_60", "mesh_61", "mesh_62", "mesh_63",
];

pub fn parse_compound_materials(
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

pub fn load_compound_materials(
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

#[cfg(test)]
mod tests {
    use bytemuck::Zeroable;
    use manifold_node_engine::mesh::MeshVertex;
    use manifold_node_engine::scene::mesh_selection::select_fragment;

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
    }
}

/// Apply a fixed authored transform, its parent frame included, to source
/// vertices before they become a collision shape. Billboard transforms are
/// camera-relative and therefore invalid for collider geometry.
pub fn transform_vertices(vertices: &mut [MeshVertex], transform: Transform) -> Result<(), String> {
    validate_transform(transform)?;
    let m = transform.world();
    for vertex in vertices {
        let p = vertex.position;
        vertex.position = std::array::from_fn(|row| {
            m[0][row] * p[0] + m[1][row] * p[1] + m[2][row] * p[2] + m[3][row]
        });
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

#[cfg(test)]
mod transform_tests {
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
                ..Default::default()
            },
        )
        .unwrap();
        assert!((vertices[0].position[0] - 2.0).abs() < 1e-6);
        assert!((vertices[0].position[1] - 5.0).abs() < 1e-6);
        assert!((vertices[0].position[2] - 4.0).abs() < 1e-6);
    }

    #[test]
    fn invalid_collider_transform_is_rejected() {
        assert!(
            validate_transform(Transform {
                scale: [f32::NAN, 1.0, 1.0],
                ..Transform::default()
            })
            .is_err()
        );
        assert!(
            validate_transform(Transform {
                billboard: true,
                ..Transform::default()
            })
            .is_err()
        );
    }
}

/// The `parts` param: how many compound parts (`mesh_k` with `part_k`) a
/// rigid body takes. A Group body has one per object inside it.
pub fn part_count(params: &ParamValues) -> usize {
    params
        .get("parts")
        .and_then(ParamValue::as_scalar)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .map_or(0, |value| value.round() as usize)
}

/// Whether `port` is one of a body's `mesh_k` part inputs.
pub fn is_part_mesh(port: &str) -> bool {
    port.strip_prefix("mesh_")
        .is_some_and(|index| !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit()))
}

/// A rigid body's part inputs, sized by its `parts` param: the node's fixed
/// inputs, then `part_k` and `mesh_k` for each part. Sized when the param
/// changes, never per frame, so a group of any size is one body.
#[derive(Default)]
pub struct PartPorts {
    inputs: Vec<NodeInput>,
    meshes: Vec<String>,
    parts: Vec<String>,
}

impl PartPorts {
    /// Size the part inputs to the `parts` param after `fixed`.
    pub fn configure(&mut self, fixed: &'static [NodeInput], params: &ParamValues) {
        let count = part_count(params);
        if !self.inputs.is_empty() && self.meshes.len() == count {
            return;
        }
        self.meshes = (0..count).map(|index| format!("mesh_{index}")).collect();
        self.parts = (0..count).map(|index| format!("part_{index}")).collect();
        self.inputs.clear();
        self.inputs.extend(fixed.iter().cloned());
        let port = |name: &String, ty| NodeInput {
            name: Cow::Owned(name.clone()),
            ty,
            kind: PortKind::Input,
            required: false,
        };
        self.inputs.extend(
            self.parts
                .iter()
                .map(|name| port(name, PortType::Transform)),
        );
        self.inputs.extend(
            self.meshes
                .iter()
                .map(|name| port(name, PortType::MeshSource)),
        );
    }

    /// The node's inputs, once configured.
    pub fn inputs(&self) -> Option<&[NodeInput]> {
        (!self.inputs.is_empty()).then_some(self.inputs.as_slice())
    }

    /// The parts this body takes.
    pub fn count(&self) -> usize {
        self.meshes.len()
    }

    /// Part `index`'s mesh and transform inputs.
    pub fn ports(&self, index: usize) -> (&str, &str) {
        (&self.meshes[index], &self.parts[index])
    }
}
