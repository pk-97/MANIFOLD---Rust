//! Reads a model file into the scene's import tree (SCENE_DOCUMENT_DESIGN.md
//! section 7.7 (Imports)): one part per material on each mesh-bearing file
//! node, empty pass-through nodes collapsed, nested where the file nests,
//! each at its file node's own transform;
//! one material per glTF material the parts use; the file's lights and
//! perspective cameras where the file puts them; and its clip count. The
//! commands in `manifold-editing` turn the tree into items; the compiler's
//! Mesh shape (`scene_compile/mesh.rs`) draws each part by its file node path
//! and material, and reads what the file holds through [`model_file`].
//!
//! No graph is built here. The parse is the one the mesh loader uses
//! (`gltf_load::parse_document_and_buffers`).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use manifold_core::effect_graph_def::{BindingTarget, EffectGraphDef, SerializedParamValue};
use manifold_core::scene_doc::{
    ImportedCamera, ImportedLight, ImportedMaterial, ImportedModel, ImportedNode, ImportedTransform, ItemId, LightType,
    MeshPrimitives, ModelFacts, transform,
};
use manifold_core::scene_schema::CLIP_PLANE;

use super::gltf_load::{self, FileNode, GltfLightKind};

/// A model file read once, on the import worker: its parts, the facts the
/// compiler reads ([`model_file`]), and what the read could not carry over.
pub struct ReadModel {
    pub model: ImportedModel,
    pub facts: ModelFacts,
    pub warnings: Vec<String>,
}

/// Parse `path` once into its parts and its facts. Errors when the file
/// cannot be read or has no mesh.
pub fn read_model(path: &Path) -> Result<ReadModel, String> {
    let (document, buffers) = gltf_load::parse_document_and_buffers(path)?;
    let model = describe(path, &document, &buffers)?;
    let (facts, warnings) = model_file::facts_of(path, &document, &buffers);
    Ok(ReadModel { model, facts: ModelFacts::new(facts), warnings })
}

/// Parse `path` and describe it as scene parts. Errors when the file cannot
/// be read or has no mesh.
pub fn import_model(path: &Path) -> Result<ImportedModel, String> {
    let (document, buffers) = gltf_load::parse_document_and_buffers(path)?;
    describe(path, &document, &buffers)
}

fn describe(path: &Path, document: &gltf::Document, buffers: &[gltf::buffer::Data]) -> Result<ImportedModel, String> {
    let tree = gltf_load::file_node_tree(document, buffers);
    let mut paths = HashMap::new();
    node_paths(&tree, &mut paths);
    let names: Vec<String> = document
        .materials()
        .enumerate()
        .map(|(i, m)| m.name().map(str::trim).filter(|n| !n.is_empty()).map_or_else(|| format!("Material {}", i + 1), str::to_owned))
        .collect();
    let mut used = BTreeSet::new();
    let roots: Vec<ImportedNode> = tree.iter().filter_map(|node| part(node, &names, &mut used)).collect();
    if roots.is_empty() {
        return Err(format!("{}: no meshes — nothing to import", path.display()));
    }
    let name = path
        .file_stem()
        .map(|stem| stem.to_string_lossy().into_owned())
        .filter(|stem| !stem.is_empty())
        .unwrap_or_else(|| "Model".to_owned());
    let radius = gltf_load::file_bounds(document, buffers).map_or(1.0, |(lo, hi)| {
        ((0..3).map(|axis| (hi[axis] - lo[axis]).powi(2)).sum::<f32>().sqrt() * 0.5).max(1e-3)
    });
    let materials = used
        .into_iter()
        .map(|material| ImportedMaterial { key: material_key(material), name: material_name(material, &names), material })
        .collect();
    let at = |index: usize| paths.get(&index).cloned().unwrap_or_else(|| format!("#{index}"));
    let lights = gltf_load::collect_punctual_lights(document)
        .into_iter()
        .enumerate()
        .map(|(i, light)| {
            let path = at(light.node_index);
            imported_light(light, i, &path, radius)
        })
        .collect();
    let (cameras, _) = gltf_load::parse_cameras(document, &gltf_load::build_parent_map(document));
    let cameras = cameras
        .into_iter()
        .enumerate()
        .map(|(i, camera)| ImportedCamera {
            key: format!("camera:{}", at(camera.node_index)),
            // An empty name would leave the camera unnamed on the Camera row.
            name: camera
                .name
                .as_deref()
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .map_or_else(|| format!("Camera {}", i + 1), str::to_owned),
            values: vec![
                ("pos_x", camera.pos[0]),
                ("pos_y", camera.pos[1]),
                ("pos_z", camera.pos[2]),
                ("yaw", camera.yaw),
                ("pitch", camera.pitch),
                ("roll", camera.roll),
                ("fov_y", camera.fov_y),
                // The file's own clip planes, held only above 0 as the rows are.
                ("near", CLIP_PLANE.clamp(camera.near, (0.0, 0.0))),
                ("far", CLIP_PLANE.clamp(camera.far, (0.0, 0.0))),
            ],
        })
        .collect();
    let clips = document
        .animations()
        .map(|clip| clip.name().map_or_else(|| format!("Clip {}", clip.index() + 1), str::to_owned))
        .collect();
    Ok(ImportedModel { name, roots, materials, lights, cameras, clips })
}

/// A `KHR_lights_punctual` light as the old importer placed it: at its
/// node's world position, aimed one model radius (at least a metre) along
/// its forward axis, with the file's photometric intensity, inverse-square
/// falloff and soft shadows.
fn imported_light(light: gltf_load::GltfPunctualLight, i: usize, path: &str, radius: f32) -> ImportedLight {
    let (kind, cones) = match light.kind {
        GltfLightKind::Directional => (LightType::Sun, None),
        GltfLightKind::Point => (LightType::Point, None),
        GltfLightKind::Spot { inner_cone_angle, outer_cone_angle } => {
            (LightType::Spot, Some((inner_cone_angle, outer_cone_angle)))
        }
    };
    let reach = radius.max(1.0);
    let aim = [0, 1, 2].map(|axis| light.world_pos[axis] + light.world_forward[axis] * reach);
    let mut values = vec![
        ("pos_x", light.world_pos[0]),
        ("pos_y", light.world_pos[1]),
        ("pos_z", light.world_pos[2]),
        ("aim_x", aim[0]),
        ("aim_y", aim[1]),
        ("aim_z", aim[2]),
        ("color_r", light.color[0]),
        ("color_g", light.color[1]),
        ("color_b", light.color[2]),
        ("intensity", light.intensity),
        ("range", light.range.unwrap_or(0.0)),
        ("cast_shadows", 1.0),
        ("shadow_softness", 1.0),
    ];
    if !matches!(kind, LightType::Sun) {
        values.push(("falloff", 1.0));
    }
    if let Some((inner, outer)) = cones {
        values.push(("inner_cone_angle", inner));
        values.push(("outer_cone_angle", outer));
    }
    ImportedLight {
        key: format!("light:{path}"),
        name: light.name.clone().unwrap_or_else(|| format!("Light {}", i + 1)),
        light: kind,
        values,
    }
}

fn node_paths(level: &[FileNode], out: &mut HashMap<usize, String>) {
    for node in level {
        out.insert(node.index, node.path.clone());
        node_paths(&node.children, out);
    }
}

fn material_key(material: Option<u32>) -> String {
    material.map_or_else(|| "material:default".to_owned(), |index| format!("material:{index}"))
}

fn material_name(material: Option<u32>, names: &[String]) -> String {
    material.and_then(|index| names.get(index as usize).cloned()).unwrap_or_else(|| "Default Material".to_owned())
}

/// The model at `path` in the base scene (minus its cube), compiled: the scene
/// a drop makes, at the file's own size and place, every item starting at
/// the file's values. For the render and soak tools, which draw a file
/// without a project around it.
pub fn compile_model_scene(path: &Path) -> Result<(EffectGraphDef, ImportedModel), String> {
    compile_model_scene_with(path, false, |_, _| {})
}

/// [`compile_model_scene`] with the import's Combine Meshes on (a
/// photoscan's setting): every part drawn where the file puts it, in the
/// import Group's unturned frame.
pub fn compile_combined_model_scene(path: &Path) -> Result<(EffectGraphDef, ImportedModel), String> {
    compile_model_scene_with(path, true, |_, _| {})
}

/// [`compile_model_scene`] with Combine Meshes `combine`, the scene first
/// changed by `edit`, which gets the import Group's id: a test adds the
/// modifiers it bends the model with.
pub fn compile_model_scene_with(
    path: &Path,
    combine: bool,
    edit: impl FnOnce(&mut manifold_core::scene_doc::SceneDoc, &ItemId),
) -> Result<(EffectGraphDef, ImportedModel), String> {
    let ReadModel { model, facts, .. } = read_model(path)?;
    let id = ItemId::new("model");
    let file = path.to_string_lossy();
    let mut doc = model.scene_doc(&file, id.clone());
    if let Some(manifold_core::scene_doc::Item { fields: manifold_core::scene_doc::ItemFields::Group(group), .. }) =
        doc.items.iter_mut().find(|item| item.id == id)
    {
        group.combine_meshes = combine;
    }
    edit(&mut doc, &id);
    let center = model_file::world_bounds(&facts).map(|(c, _)| c);
    doc.models.insert(file.into_owned(), facts);
    let mut def = manifold_core::scene_doc::compile(&doc)?;
    for (item, property, value) in model.starting_values(&id, center) {
        start_at(&mut def, &format!("{item}.{property}"), value);
    }
    Ok((def, model))
}

/// Start the card param `param_id` at `value`, as an edit would: its spec,
/// its bindings and every node it drives, each node param keeping its type.
fn start_at(def: &mut EffectGraphDef, param_id: &str, value: f32) {
    let Some(meta) = def.preset_metadata.as_mut() else {
        return;
    };
    let Some(spec) = meta.params.iter_mut().find(|p| p.id == param_id) else {
        return;
    };
    spec.default_value = value;
    let mut targets = Vec::new();
    for binding in meta.bindings.iter_mut().filter(|b| b.id == param_id) {
        binding.default_value = value;
        if let BindingTarget::Node { node_id, param } = &binding.target {
            targets.push((node_id.clone(), param.clone()));
        }
    }
    for (node_id, param) in targets {
        let Some(node) = def.nodes.iter_mut().find(|n| n.node_id == node_id) else {
            continue;
        };
        let typed = match node.params.get(&param) {
            Some(SerializedParamValue::Int { .. }) => SerializedParamValue::Int { value: value.round() as i32 },
            Some(SerializedParamValue::Bool { .. }) => SerializedParamValue::Bool { value: value > 0.5 },
            Some(SerializedParamValue::Enum { .. }) => SerializedParamValue::Enum { value: value.max(0.0).round() as u32 },
            _ => SerializedParamValue::Float { value },
        };
        node.params.insert(param, typed);
    }
}

/// The part a file node becomes, or none when nothing under it has a mesh,
/// at the node's own transform (Blender's glTF import). A node with no
/// mesh and one part below it is that part, its transform folded into the
/// part's; its path stays the part's key, so the id survives the collapse.
/// When the fold has no exact position, rotation and scale (a turned part
/// under an unevenly scaled node), the node stays a group. A mesh of
/// several materials is one Object per material, keyed by the material,
/// under a group at the node's transform.
fn part(node: &FileNode, names: &[String], used: &mut BTreeSet<Option<u32>>) -> Option<ImportedNode> {
    let mut children: Vec<ImportedNode> = node.children.iter().filter_map(|child| part(child, names, used)).collect();
    let (position, rotation, scale) = transform::decompose(node.local);
    let own = ImportedTransform { position, rotation, scale };
    let several = node.parts.len() > 1;
    let mut objects: Vec<ImportedNode> = node
        .parts
        .iter()
        .map(|file_part| {
            used.insert(file_part.material);
            let (key, name, primitives) = if several {
                let primitives = file_part
                    .material
                    .map_or(MeshPrimitives::NoMaterial, |index| MeshPrimitives::Material { index });
                let tag = file_part.material.map_or_else(|| "default".to_owned(), |index| index.to_string());
                let name = format!("{} ({})", node.name, material_name(file_part.material, names));
                (format!("{}#mesh:{tag}", node.path), name, primitives)
            } else {
                (format!("{}#mesh", node.path), node.name.clone(), MeshPrimitives::All)
            };
            ImportedNode {
                key,
                name,
                mesh_node: Some(node.path.clone()),
                primitives,
                material: Some(material_key(file_part.material)),
                vertex_count: file_part.vertex_count,
                transform: ImportedTransform::IDENTITY,
                children: Vec::new(),
            }
        })
        .collect();
    let group = |children: Vec<ImportedNode>| ImportedNode {
        key: node.path.clone(),
        name: node.name.clone(),
        mesh_node: None,
        primitives: MeshPrimitives::All,
        material: None,
        vertex_count: 0,
        transform: own,
        children,
    };
    match (objects.len(), children.len()) {
        (0, 0) => None,
        (0, 1) => {
            let mut only = children.pop()?;
            let child = only.transform;
            match folded(transform::mat4_mul(node.local, transform::trs_matrix(child.position, child.rotation, child.scale))) {
                Some(transform) => {
                    only.transform = transform;
                    Some(only)
                }
                None => Some(group(vec![only])),
            }
        }
        (1, 0) => objects.pop().map(|mut only| {
            only.transform = own;
            only
        }),
        _ => {
            objects.append(&mut children);
            Some(group(objects))
        }
    }
}

/// `m` as a position, rotation and scale, when those rebuild it: a node's
/// transform folded into its only part's.
fn folded(m: transform::Mat4) -> Option<ImportedTransform> {
    let (position, rotation, scale) = transform::decompose(m);
    let rebuilt = transform::trs_matrix(position, rotation, scale);
    let size = m.iter().flatten().fold(1.0f32, |size, value| size.max(value.abs()));
    let exact = m.iter().flatten().zip(rebuilt.iter().flatten()).all(|(a, b)| (a - b).abs() <= 1e-5 * size);
    exact.then_some(ImportedTransform { position, rotation, scale })
}

pub(crate) mod model_file;
pub use model_file::{read_facts, read_file_facts, world_bounds};

#[cfg(test)]
mod tests;
