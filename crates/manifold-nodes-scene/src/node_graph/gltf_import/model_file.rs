//! A model file as the scene compiler reads it (SCENE_DOCUMENT_DESIGN.md
//! section 7.7 (Imports)): its materials with every map and extension, its
//! texture sizes, its size, and per file node the skin, morph and animation
//! facts an imported part compiles from. The document names the file and
//! the node; these facts stay in the file.
//!
//! Read once per file on the import worker ([`read_facts`], or with the
//! import itself) and carried on the document (`SceneDoc::models`), so no
//! compile on the UI or content thread opens a file. A file whose facts have
//! not arrived compiles as plain meshes with the default surface. No image
//! is decoded: texture sizes come from the image headers.

use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use manifold_core::scene_doc::{ModelFacts, SceneDoc};

use crate::node_graph::gltf_load::{
    self, FileNode, FilePart, GltfMaterialInfo, GltfNodeAnimation, GltfSkinInfo,
};

/// Per-clip `[clip_index, duration_s]` rows, and the first clip's duration
/// for the clock's pre-load fallback.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ClipDurations {
    pub rows: Vec<Vec<f32>>,
    pub first: f32,
}

/// How a part moves.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Rig {
    /// Skinned against glTF skin `skin_index`.
    Skin { skin_index: u32, joint_count: u32, clips: ClipDurations },
    /// Not skinned, but its node or an ancestor animates: the node's whole
    /// chain drives it as a one-slot palette.
    Chain { node_index: u32, clips: ClipDurations },
}

/// One part's morph targets.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Morph {
    pub node_index: u32,
    pub target_count: u32,
    /// `[target_index, weight]` rows: the mesh's static weights.
    pub static_weights: Vec<Vec<f32>>,
    pub clips: ClipDurations,
}

/// One file node's facts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct NodeFacts {
    pub parts: Vec<FilePart>,
    pub rig: Option<Rig>,
    /// Morph targets per material part (`None` key is the default material).
    pub morphs: Vec<(Option<u32>, Morph)>,
}

impl NodeFacts {
    /// The part drawing `material` (`None` the default material), or the
    /// only part when the selection is every primitive.
    pub(crate) fn part(&self, material: Option<Option<u32>>) -> Option<&FilePart> {
        match material {
            Some(material) => self.parts.iter().find(|part| part.material == material),
            None => self.parts.first(),
        }
    }

    pub(crate) fn morph(&self, material: Option<u32>) -> Option<&Morph> {
        self.morphs.iter().find(|(key, _)| *key == material).map(|(_, morph)| morph)
    }
}

/// What the compiler reads from one model file.
#[derive(Debug, Clone)]
pub(crate) struct ModelFile {
    /// By glTF material index.
    pub materials: Vec<GltfMaterialInfo>,
    pub default_material: GltfMaterialInfo,
    /// By texture index.
    pub texture_dims: Vec<(u32, u32)>,
    /// Half the diagonal of the file's world box: the radius every mesh
    /// source records, which mesh modifiers measure the model by.
    pub radius: f32,
    /// By file node path.
    pub nodes: HashMap<String, NodeFacts>,
}

impl ModelFile {
    /// The material `material` (`None` the glTF default material).
    pub(crate) fn material(&self, material: Option<u32>) -> &GltfMaterialInfo {
        material.and_then(|index| self.materials.get(index as usize)).unwrap_or(&self.default_material)
    }
}

/// The facts of the model at `path` the document carries, if they have
/// arrived.
pub(crate) fn model_file<'d>(doc: &'d SceneDoc, path: &str) -> Option<&'d ModelFile> {
    doc.models.get(path)?.get::<ModelFile>()
}

/// Read the facts of the model at `path`: on the import worker, never on the
/// UI or content thread. With them, what the file's materials hold that the
/// surface cannot draw, for the import warnings.
pub fn read_facts(path: &Path) -> Result<(ModelFacts, Vec<String>), String> {
    let (document, buffers) = gltf_load::parse_document_and_buffers(path)?;
    let (facts, warnings) = facts_of(path, &document, &buffers);
    Ok((ModelFacts::new(facts), warnings))
}

/// A picture file's pixel size, read from its header on the worker: what a
/// Material's file texture compiles with.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PictureSize(pub u32, pub u32);

/// The size of the picture at `path` the document carries, if it has arrived.
pub(crate) fn picture_size(doc: &SceneDoc, path: &str) -> Option<(u32, u32)> {
    doc.models.get(path)?.get::<PictureSize>().map(|size| (size.0, size.1))
}

/// Read the facts of a file a document names: a picture's size, or a
/// model's facts ([`read_facts`]). On the worker only.
pub fn read_file_facts(path: &Path) -> Result<(ModelFacts, Vec<String>), String> {
    if image::ImageFormat::from_path(path).is_err() {
        return read_facts(path);
    }
    let (width, height) = gltf_load::picture_file_dims(path).ok_or_else(|| format!("{} is not a picture this can read", path.display()))?;
    Ok((ModelFacts::new(PictureSize(width, height)), Vec::new()))
}

/// The facts of a parsed model file, and its material report lines.
pub(crate) fn facts_of(path: &Path, document: &gltf::Document, buffers: &[gltf::buffer::Data]) -> (ModelFile, Vec<String>) {
    let mut report = Vec::new();
    let materials = document
        .materials()
        .map(|m| {
            let label = m.name().map_or_else(|| format!("material {}", m.index().unwrap_or(0)), str::to_owned);
            gltf_load::material_info(document, &m, &label, &mut report)
        })
        .collect();
    let radius = gltf_load::file_bounds(document, buffers).map_or(1e-3, |(lo, hi)| {
        let diagonal = (0..3).map(|axis| (hi[axis] - lo[axis]).powi(2)).sum::<f32>().sqrt();
        (diagonal * 0.5).max(1e-3)
    });
    let animations = gltf_load::parse_animations(document, buffers);
    let by_clip: Vec<BTreeMap<usize, GltfNodeAnimation>> =
        animations.iter().map(|clip| clip.nodes.iter().map(|n| (n.node_index, n.clone())).collect()).collect();
    let skins = gltf_load::parse_skins(document, buffers);
    let mut nodes = HashMap::new();
    let tree = gltf_load::file_node_tree(document, buffers);
    collect(&tree, &[], document, &skins, &by_clip, &mut nodes);
    let facts = ModelFile {
        materials,
        default_material: gltf_load::default_material_info(),
        texture_dims: gltf_load::texture_dims(path, document, buffers),
        radius,
        nodes,
    };
    (facts, report)
}

fn collect(
    level: &[FileNode],
    ancestors: &[usize],
    document: &gltf::Document,
    skins: &[GltfSkinInfo],
    by_clip: &[BTreeMap<usize, GltfNodeAnimation>],
    out: &mut HashMap<String, NodeFacts>,
) {
    for file_node in level {
        let mut chain = ancestors.to_vec();
        chain.push(file_node.index);
        if file_node.has_mesh
            && let Some(node) = document.nodes().nth(file_node.index)
        {
            let rig = match node.skin() {
                Some(skin) => skins.get(skin.index()).map(|info| Rig::Skin {
                    skin_index: skin.index() as u32,
                    joint_count: info.joint_node_indices.len() as u32,
                    clips: skeleton_pose_clip_durations(info, by_clip),
                }),
                None => chain_animated(&chain, by_clip)
                    .then(|| Rig::Chain { node_index: file_node.index as u32, clips: rigid_clip_durations(by_clip) }),
            };
            let morphs = file_node
                .parts
                .iter()
                .filter_map(|part| morph_of(&node, file_node.index, part.material, by_clip).map(|m| (part.material, m)))
                .collect();
            out.insert(file_node.path.clone(), NodeFacts { parts: file_node.parts.clone(), rig, morphs });
        }
        collect(&file_node.children, &chain, document, skins, by_clip, out);
    }
}

/// Whether any node on `chain` (the node and its ancestors) has a
/// translation, rotation or scale channel in any clip.
fn chain_animated(chain: &[usize], by_clip: &[BTreeMap<usize, GltfNodeAnimation>]) -> bool {
    chain.iter().any(|node| {
        by_clip.iter().any(|clip| {
            clip.get(node)
                .is_some_and(|anim| anim.translation.is_some() || anim.rotation.is_some() || anim.scale.is_some())
        })
    })
}

/// The morph targets of `node`'s primitives of `material`: the target count
/// of the first such primitive that has targets.
fn morph_of(
    node: &gltf::Node,
    node_index: usize,
    material: Option<u32>,
    by_clip: &[BTreeMap<usize, GltfNodeAnimation>],
) -> Option<Morph> {
    let mesh = node.mesh()?;
    let target_count = mesh
        .primitives()
        .find(|p| p.material().index().map(|i| i as u32) == material && p.morph_targets().len() > 0)
        .map(|p| p.morph_targets().len())?;
    let weights = mesh.weights().unwrap_or(&[]);
    let static_weights =
        (0..target_count).map(|i| vec![i as f32, weights.get(i).copied().unwrap_or(0.0)]).collect();
    Some(Morph {
        node_index: node_index as u32,
        target_count: target_count as u32,
        static_weights,
        clips: morph_clip_durations(node_index, by_clip),
    })
}

/// Per-clip durations of a skin: the last keyframe of any joint channel,
/// floored at 1 ms. A file with no clips is one static clip 0.
fn skeleton_pose_clip_durations(skin: &GltfSkinInfo, by_clip: &[BTreeMap<usize, GltfNodeAnimation>]) -> ClipDurations {
    durations(by_clip, |clip| {
        skin.joint_node_indices
            .iter()
            .filter_map(|joint| clip.get(joint))
            .map(trs_end)
            .fold(0.0, f32::max)
    })
}

/// Per-clip durations for a one-slot palette: the last keyframe of every
/// channel in the clip, ancestors included, since the pose samples the
/// whole hierarchy on that clock.
fn rigid_clip_durations(by_clip: &[BTreeMap<usize, GltfNodeAnimation>]) -> ClipDurations {
    durations(by_clip, |clip| {
        clip.values()
            .map(|anim| trs_end(anim).max(anim.weights.as_ref().map_or(0.0, |w| last(&w.times))))
            .fold(0.0, f32::max)
    })
}

/// Per-clip durations of a morph: the last keyframe of the node's weights
/// channel.
fn morph_clip_durations(node_index: usize, by_clip: &[BTreeMap<usize, GltfNodeAnimation>]) -> ClipDurations {
    durations(by_clip, |clip| {
        clip.get(&node_index).and_then(|anim| anim.weights.as_ref()).map_or(0.0, |w| last(&w.times))
    })
}

fn durations(
    by_clip: &[BTreeMap<usize, GltfNodeAnimation>],
    duration_of: impl Fn(&BTreeMap<usize, GltfNodeAnimation>) -> f32,
) -> ClipDurations {
    let empty = BTreeMap::new();
    let rows: Vec<Vec<f32>> = (0..by_clip.len().max(1))
        .map(|c| vec![c as f32, duration_of(by_clip.get(c).unwrap_or(&empty)).max(1e-3)])
        .collect();
    let first = rows[0][1];
    ClipDurations { rows, first }
}

fn trs_end(anim: &GltfNodeAnimation) -> f32 {
    anim.translation
        .as_ref()
        .map_or(0.0, |t| last(&t.times))
        .max(anim.rotation.as_ref().map_or(0.0, |r| last(&r.times)))
        .max(anim.scale.as_ref().map_or(0.0, |s| last(&s.times)))
}

fn last(times: &[f32]) -> f32 {
    times.last().copied().unwrap_or(0.0)
}
