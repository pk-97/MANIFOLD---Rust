//! Material map-texture wiring: one glTF texture map (normal / MR /
//! occlusion / emissive / …) into an object's `node.scene_object` input.
//!
//! P3-D T1: the eighteen near-identical map families are a catalog — one row
//! per glTF texture map, differing only in which source field carries the
//! texture index, the decode colour space, the node/port names, and (for the
//! spec-gloss `mrMap` case alone) a channel repack mode. That catalog is
//! [`MAP_FAMILIES`], walked once by [`wire_map_families`]. The base-colour map
//! is NOT a row — see the explicit block in `object_group.rs`, which alone
//! increments `textures_wired` and pre-dates the per-object texture cache
//! (RENDERER_RUNTIME_DECOMPOSITION_DESIGN.md D8, both oddities preserved).

use manifold_core::effect_graph_def::{
    BindingTarget, EffectGraphNode, EffectGraphWire, SerializedParamValue, StringBindingDef,
};
use manifold_core::NodeId;

use crate::node_graph::gltf_load::{
    GltfFilterMode, GltfMaterialInfo, GltfSamplerInfo, GltfWrapMode,
};
use crate::node_graph::material::MapSamplerDesc;

use super::assembly::{enum_val, float, int, plain_node, wire};
use super::MODEL_FILE_PARAM_ID;

/// The per-object accumulators a map-family wire threads through: the running
/// node/wire/string-binding vectors, the id source, the object's index and
/// glb path, its `node.scene_object` id, and the per-object texture-reuse
/// cache. Bundled so the walk stays callable without an 11-argument helper;
/// this is P3-A's `ObjectAssembly` seam, introduced early because T1's walk
/// needs it (D9).
pub(super) struct ObjectAssembly<'a> {
    /// This object's index within the import (`k`) — names the source nodes.
    pub k: usize,
    /// The glb file path, bound onto each source node's `path` param.
    pub path_str: &'a str,
    /// Fresh numeric node-id source (a counter — importer output stays
    /// deterministic).
    pub fresh_id: &'a mut dyn FnMut() -> u32,
    /// Nodes created for this object's group.
    pub group_nodes: &'a mut Vec<EffectGraphNode>,
    /// Wires created for this object's group.
    pub group_wires: &'a mut Vec<EffectGraphWire>,
    /// Outer-card Model File → source-node `path` string bindings.
    pub string_bindings: &'a mut Vec<StringBindingDef>,
    /// Per-object texture-source reuse cache, keyed by
    /// `(texture_index, color_space, channel_mode)` — see [`wire_map_family`].
    pub tex_cache: &'a mut std::collections::HashMap<(u32, u32, u32), (u32, String)>,
    /// This object's `node.scene_object` numeric id — the wire destination.
    pub scene_object_id: u32,
    /// Decoded pixel dimensions per glTF texture index — stamped onto each
    /// source node so maps keep their authored resolution (was: hardcoded
    /// 1024² v1 default, which downsampled every 4K store-asset atlas).
    pub texture_dims: &'a [(u32, u32)],
}

/// One glTF texture-map family: the facts that distinguish it from the other
/// fifteen. Adding a new KHR texture family = adding one row to
/// [`MAP_FAMILIES`], never another copy of the wiring.
struct MapFamily {
    /// Which source field carries this family's texture index (`None` → the
    /// material does not use this map, and the walk skips it).
    texture: fn(&GltfMaterialInfo) -> Option<u32>,
    /// Decode colour space: 0 = sRGB (colour maps), 1 = Linear (data maps),
    /// per each family's own KHR spec section (cited per row below).
    color_space: u32,
    /// Source-node id prefix, e.g. `"normal_tex"`.
    node_prefix: &'static str,
    /// `node.scene_object` input port name, e.g. `"normal_map"`
    /// (SCENE_OBJECT_AND_PANEL_V2_DESIGN.md D1/D3 — snake_case).
    port: &'static str,
    /// Channel-repack mode: 0 = passthrough for every family except the
    /// spec-gloss `mrMap` stand-in, which repacks its alpha (gloss) into
    /// G=roughness/B=metallic (GLB_XFAIL_BURNDOWN_DESIGN.md D2).
    channel_mode: fn(&GltfMaterialInfo) -> u32,
}

/// The eighteen map families, in wiring order. Colour-space citations per KHR
/// section: data maps (normal / MR / occlusion / sheen-roughness /
/// iridescence(+thickness) / anisotropy / clearcoat(+roughness/+normal) /
/// specular / transmission / diffuse-transmission-factor / volume-thickness)
/// decode Linear — the raw bytes ARE the value; colour maps (emissive /
/// sheenColor / specularColor / diffuseTransmissionColor) decode sRGB, same
/// convention as base-colour.
const MAP_FAMILIES: &[MapFamily] = &[
    // D3/D5/D6 — the base five maps (base-colour is the separate explicit
    // block in object_group.rs). ORM-packed files (occlusion index == mr
    // index) reuse one source via the cache.
    MapFamily {
        texture: |m| m.normal_texture,
        color_space: 1, // Linear — tangent-space normal map (data)
        node_prefix: "normal_tex",
        port: "normal_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.mr_texture,
        color_space: 1, // Linear — metallic-roughness (data)
        node_prefix: "mr_tex",
        port: "mr_map",
        // GLB_XFAIL_BURNDOWN_DESIGN.md D2: a spec-gloss
        // specularGlossinessTexture standing in for mrMap repacks its alpha
        // (gloss) into G=roughness/B=metallic; every other family passes 0.
        channel_mode: |m| {
            if m.mr_texture_is_gloss_alpha {
                1
            } else {
                0
            }
        },
    },
    MapFamily {
        texture: |m| m.occlusion_texture,
        color_space: 1, // Linear — ambient occlusion (data)
        node_prefix: "occlusion_tex",
        port: "occlusion_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.emissive_texture,
        color_space: 0, // sRGB — a colour map, same convention as base-colour
        node_prefix: "emissive_tex",
        port: "emissive_map",
        channel_mode: |_| 0,
    },
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E3/E4/E5 — sheen / iridescence /
    // anisotropy extension textures.
    MapFamily {
        texture: |m| m.sheen_color_texture,
        color_space: 0, // sRGB — sheenColorTexture is a colour map
        node_prefix: "sheen_color_tex",
        port: "sheen_color_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.sheen_roughness_texture,
        color_space: 1, // Linear — data map (alpha channel)
        node_prefix: "sheen_roughness_tex",
        port: "sheen_roughness_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.iridescence_texture,
        color_space: 1, // Linear — data map (R channel = factor scale)
        node_prefix: "iridescence_tex",
        port: "iridescence_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.iridescence_thickness_texture,
        color_space: 1, // Linear — data map (G channel = thickness lerp)
        node_prefix: "iridescence_thickness_tex",
        port: "iridescence_thickness_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.anisotropy_texture,
        color_space: 1, // Linear — data map (RG = rotation, B = strength)
        node_prefix: "anisotropy_tex",
        port: "anisotropy_map",
        channel_mode: |_| 0,
    },
    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 — the texture-completion sweep:
    // clearcoat / specular / transmission / volume-thickness textures.
    MapFamily {
        texture: |m| m.clearcoat_texture,
        color_space: 1, // Linear — data map (R channel = clearcoatFactor scale)
        node_prefix: "clearcoat_tex",
        port: "clearcoat_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.clearcoat_roughness_texture,
        color_space: 1, // Linear — data map (G channel = clearcoatRoughnessFactor scale)
        node_prefix: "clearcoat_roughness_tex",
        port: "clearcoat_roughness_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.clearcoat_normal_texture,
        color_space: 1, // Linear — tangent-space normal map, same convention as normalMap
        node_prefix: "clearcoat_normal_tex",
        port: "clearcoat_normal_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.specular_texture,
        color_space: 1, // Linear — data map (ALPHA channel = specularFactor scale)
        node_prefix: "specular_tex",
        port: "specular_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.specular_color_texture,
        color_space: 0, // sRGB — specularColorTexture is a colour map
        node_prefix: "specular_color_tex",
        port: "specular_color_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.transmission_texture,
        color_space: 1, // Linear — data map (R channel = transmissionFactor scale)
        node_prefix: "transmission_tex",
        port: "transmission_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.diffuse_transmission_texture,
        color_space: 1, // Linear — data map (A channel = diffuseTransmissionFactor scale)
        node_prefix: "diffuse_transmission_tex",
        port: "diffuse_transmission_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.diffuse_transmission_color_texture,
        color_space: 0, // sRGB — diffuseTransmissionColorTexture is a colour map
        node_prefix: "diffuse_transmission_color_tex",
        port: "diffuse_transmission_color_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.volume_thickness_texture,
        color_space: 1, // Linear — data map (G channel = thicknessFactor scale)
        node_prefix: "volume_thickness_tex",
        port: "volume_thickness_map",
        channel_mode: |_| 0,
    },
];

/// Walk [`MAP_FAMILIES`] once, wiring every map this material uses into the
/// object's group (`asm`). The base-colour map is handled by the explicit
/// block in `object_group.rs`, not here.
pub(super) fn wire_map_families(m: &GltfMaterialInfo, asm: &mut ObjectAssembly<'_>) {
    for family in MAP_FAMILIES {
        wire_map_family(family, m, asm);
    }
}

/// Wire one glTF map texture into this object's group: creates a
/// `node.gltf_texture_source` (or reuses one already created for the same
/// `(texture_index, color_space, channel_mode)` within this object — D5's
/// ORM-packing case, where occlusion and metallic-roughness are the same
/// physical image), wires the source directly into the object's
/// `node.scene_object` input named `family.port`, and adds the outer-card
/// Model File → source-node `path` string binding. A `None` texture field
/// means the material does not use this family — nothing is wired.
fn wire_map_family(family: &MapFamily, m: &GltfMaterialInfo, asm: &mut ObjectAssembly<'_>) {
    let Some(tex_index) = (family.texture)(m) else {
        return;
    };
    let color_space = family.color_space;
    let channel_mode = (family.channel_mode)(m);

    // GLTF_MATERIAL_EXTENSIONS_DESIGN.md E6 bugfix: the cache key MUST
    // include `color_space`/`channel_mode`, not just `tex_index` — the base
    // five maps only ever reuse a shared texture index under the SAME
    // decode (e.g. ORM occlusion+mr both linear), but `KHR_materials_
    // specular`'s `specularTexture` (linear, alpha channel) and
    // `specularColorTexture` (sRGB, rgb channels) can legally reference the
    // SAME physical image with DIFFERENT decodes (CompareSpecular.glb does
    // exactly this) — a tex_index-only key would silently reuse the first
    // decode for both ports and corrupt the second one.
    let cache_key = (tex_index, color_space, channel_mode);
    let (node_numeric_id, _node_id_str) = if let Some(existing) = asm.tex_cache.get(&cache_key) {
        existing.clone()
    } else {
        let node_id_str = format!("{}_{}", family.node_prefix, asm.k);
        let tid = (asm.fresh_id)();
        let mut node = plain_node(tid, &node_id_str, "node.gltf_texture_source", &node_id_str);
        node.params
            .insert("texture_index".to_string(), int(tex_index as i32));
        node.params
            .insert("color_space".to_string(), enum_val(color_space));
        // GLB_XFAIL_BURNDOWN_DESIGN.md D2: 1 = gloss_to_roughness, wired
        // only for a specularGlossinessTexture standing in for `mrMap` (the
        // mr row's `channel_mode`); every other family passes 0
        // (passthrough), byte-identical to before this param existed.
        node.params
            .insert("mode".to_string(), enum_val(channel_mode));
        // Spec-gloss alpha stores glossiness. The source blit applies the
        // authored factor exactly once: roughness = 1 - factor * alpha.
        let glossiness_factor = if channel_mode == 1 {
            (1.0 - m.roughness).clamp(0.0, 1.0)
        } else {
            1.0
        };
        node.params.insert(
            "glossiness_factor".to_string(),
            float(glossiness_factor),
        );
        // Authored resolution, not the 1024² v1 default (see
        // `ObjectAssembly::texture_dims`). 8192 is the param's declared
        // range max — a past-cap image resamples rather than clamping at
        // param load.
        let (tw, th) = asm
            .texture_dims
            .get(tex_index as usize)
            .copied()
            .unwrap_or((1024, 1024));
        node.params
            .insert("width".to_string(), int((tw.min(8192)) as i32));
        node.params
            .insert("height".to_string(), int((th.min(8192)) as i32));
        asm.group_nodes.push(node);

        asm.string_bindings.push(StringBindingDef {
            id: MODEL_FILE_PARAM_ID.to_string(),
            label: "Model File".to_string(),
            default_value: asm.path_str.to_string(),
            target: BindingTarget::Node {
                node_id: NodeId::new(&node_id_str),
                param: "path".to_string(),
            },
        });

        let entry = (tid, node_id_str);
        asm.tex_cache.insert(cache_key, entry.clone());
        entry
    };

    asm.group_wires.push(wire(
        node_numeric_id,
        "out",
        asm.scene_object_id,
        family.port,
    ));
}

/// The six components of a `KHR_texture_transform` affine, in the order the
/// per-map UV-transform params are named (`{prefix}m00` … `{prefix}ty`). Used
/// by the per-map UV-transform loop in `object_group.rs`, which pairs each
/// component name with the matching value off a borrowed `&m.<map>_uv_transform`
/// (so the loop's `(prefix, transform)` pairs cannot themselves be const).
pub(super) const UV_TRANSFORM_PARTS: [&str; 6] = ["m00", "m01", "m10", "m11", "tx", "ty"];

/// One plain glTF-field → `node.pbr_material` param: a name and the extractor
/// that reads its value straight off `GltfMaterialInfo`. The five COMPUTED
/// params (color_a from `effective_alpha`, the roughness clamp, the const
/// ambient floor, the gated emission_intensity, and the alpha_mode enum) are
/// NOT rows — they stay explicit code adjacent to [`write_material_params`]'s
/// call site in `object_group.rs` (RENDERER_RUNTIME_DECOMPOSITION_DESIGN.md
/// D8: a closure-captured local smuggled into a "const" table is the tell
/// that a row isn't a fact).
struct MaterialParam {
    name: &'static str,
    value: fn(&GltfMaterialInfo) -> SerializedParamValue,
}

/// The plain field → param catalog. Extension factor defaults reproduce
/// glTF's own implicit defaults (ior=1.5, specular=1.0, clearcoat=0.0, …), so
/// a material without an extension writes byte-identical params — see
/// `gltf_load.rs` and GLTF_MATERIAL_EXTENSIONS_DESIGN.md E1 / GLB_CONFORMANCE
/// _DESIGN.md G-P4/G-P5. Insertion order is irrelevant: `node.params` is a
/// `BTreeMap`, so output is key-sorted regardless of walk order.
const MATERIAL_PARAMS: &[MaterialParam] = &[
    MaterialParam {
        name: "color_r",
        value: |m| float(m.base_color_factor[0]),
    },
    MaterialParam {
        name: "color_g",
        value: |m| float(m.base_color_factor[1]),
    },
    MaterialParam {
        name: "color_b",
        value: |m| float(m.base_color_factor[2]),
    },
    MaterialParam {
        name: "metallic",
        value: |m| float(m.metallic),
    },
    MaterialParam {
        name: "normal_scale",
        value: |m| float(m.normal_scale),
    },
    MaterialParam {
        name: "clearcoat_normal_scale",
        value: |m| float(m.clearcoat_normal_scale),
    },
    MaterialParam {
        name: "occlusion_strength",
        value: |m| float(m.occlusion_strength),
    },
    MaterialParam {
        name: "emission_r",
        value: |m| float(m.emissive[0]),
    },
    MaterialParam {
        name: "emission_g",
        value: |m| float(m.emissive[1]),
    },
    MaterialParam {
        name: "emission_b",
        value: |m| float(m.emissive[2]),
    },
    MaterialParam {
        name: "alpha_cutoff",
        value: |m| float(m.alpha_cutoff),
    },
    MaterialParam {
        name: "ior",
        value: |m| float(m.ior),
    },
    MaterialParam {
        name: "specular",
        value: |m| float(m.specular_factor),
    },
    MaterialParam {
        name: "specular_tint_r",
        value: |m| float(m.specular_color_factor[0]),
    },
    MaterialParam {
        name: "specular_tint_g",
        value: |m| float(m.specular_color_factor[1]),
    },
    MaterialParam {
        name: "specular_tint_b",
        value: |m| float(m.specular_color_factor[2]),
    },
    MaterialParam {
        name: "clearcoat",
        value: |m| float(m.clearcoat_factor),
    },
    MaterialParam {
        name: "clearcoat_roughness",
        value: |m| float(m.clearcoat_roughness_factor),
    },
    MaterialParam {
        name: "sheen_color_r",
        value: |m| float(m.sheen_color_factor[0]),
    },
    MaterialParam {
        name: "sheen_color_g",
        value: |m| float(m.sheen_color_factor[1]),
    },
    MaterialParam {
        name: "sheen_color_b",
        value: |m| float(m.sheen_color_factor[2]),
    },
    MaterialParam {
        name: "sheen_roughness",
        value: |m| float(m.sheen_roughness_factor),
    },
    MaterialParam {
        name: "iridescence",
        value: |m| float(m.iridescence_factor),
    },
    MaterialParam {
        name: "iridescence_ior",
        value: |m| float(m.iridescence_ior),
    },
    MaterialParam {
        name: "iridescence_thickness_min",
        value: |m| float(m.iridescence_thickness_minimum),
    },
    MaterialParam {
        name: "iridescence_thickness_max",
        value: |m| float(m.iridescence_thickness_maximum),
    },
    MaterialParam {
        name: "anisotropy_strength",
        value: |m| float(m.anisotropy_strength),
    },
    MaterialParam {
        name: "anisotropy_rotation",
        value: |m| float(m.anisotropy_rotation),
    },
    MaterialParam {
        name: "dispersion",
        value: |m| float(m.dispersion),
    },
    MaterialParam {
        name: "transmission",
        value: |m| float(m.transmission_factor),
    },
    MaterialParam {
        name: "volume_thickness",
        value: |m| float(m.volume_thickness_factor),
    },
    MaterialParam {
        name: "volume_attenuation_distance",
        value: |m| float(m.volume_attenuation_distance),
    },
    MaterialParam {
        name: "volume_attenuation_color_r",
        value: |m| float(m.volume_attenuation_color[0]),
    },
    MaterialParam {
        name: "volume_attenuation_color_g",
        value: |m| float(m.volume_attenuation_color[1]),
    },
    MaterialParam {
        name: "volume_attenuation_color_b",
        value: |m| float(m.volume_attenuation_color[2]),
    },
    // RAYTRACING_DESIGN.md section 16 TL3: KHR_materials_diffuse_transmission
    // factor/colour values -> pbr_material's translucency params. Texture
    // maps are wired through MAP_FAMILIES below.
    MaterialParam {
        name: "translucency",
        value: |m| float(m.diffuse_transmission_factor),
    },
    MaterialParam {
        name: "translucency_color_r",
        value: |m| float(m.diffuse_transmission_color[0]),
    },
    MaterialParam {
        name: "translucency_color_g",
        value: |m| float(m.diffuse_transmission_color[1]),
    },
    MaterialParam {
        name: "translucency_color_b",
        value: |m| float(m.diffuse_transmission_color[2]),
    },
];

/// Write the plain [`MATERIAL_PARAMS`] catalog onto a `node.pbr_material`
/// node. The five computed params stay explicit at the call site.
pub(super) fn write_material_params(mat_node: &mut EffectGraphNode, m: &GltfMaterialInfo) {
    for param in MATERIAL_PARAMS {
        mat_node
            .params
            .insert(param.name.to_string(), (param.value)(m));
    }
    write_map_metadata(mat_node, m);
}

const EXTENSION_MAP_PREFIXES: [&str; 14] = [
    "sheen_color",
    "sheen_roughness",
    "iridescence",
    "iridescence_thickness",
    "anisotropy",
    "clearcoat",
    "clearcoat_roughness",
    "clearcoat_normal",
    "specular",
    "specular_color",
    "transmission",
    "volume_thickness",
    "diffuse_transmission",
    "diffuse_transmission_color",
];

fn write_map_metadata(mat_node: &mut EffectGraphNode, m: &GltfMaterialInfo) {
    let core = [
        (
            "uv_",
            "",
            "uv_set",
            &m.base_color_uv_transform,
            m.core_tex_coords[0],
            sampler_desc(m.base_color_sampler),
        ),
        (
            "nrm_uv_",
            "nrm_",
            "nrm_uv_set",
            &m.normal_uv_transform,
            m.core_tex_coords[1],
            sampler_desc(m.normal_sampler),
        ),
        (
            "mr_uv_",
            "mr_",
            "mr_uv_set",
            &m.mr_uv_transform,
            m.core_tex_coords[2],
            sampler_desc(m.mr_sampler),
        ),
        (
            "occ_uv_",
            "occ_",
            "occ_uv_set",
            &m.occlusion_uv_transform,
            m.core_tex_coords[3],
            sampler_desc(m.occlusion_sampler),
        ),
        (
            "em_uv_",
            "em_",
            "em_uv_set",
            &m.emissive_uv_transform,
            m.core_tex_coords[4],
            sampler_desc(m.emissive_sampler),
        ),
    ];
    for (uv_prefix, sampler_prefix, tex_coord_name, transform, tex_coord, sampler) in core {
        for (part, value) in UV_TRANSFORM_PARTS.iter().zip(transform.iter()) {
            mat_node
                .params
                .insert(format!("{uv_prefix}{part}"), float(*value));
        }
        insert_sampler_params(mat_node, sampler_prefix, sampler, tex_coord_name, tex_coord);
    }
    for (prefix, info) in EXTENSION_MAP_PREFIXES.iter().zip(m.extension_maps.iter()) {
        for (part, value) in UV_TRANSFORM_PARTS.iter().zip(info.uv_transform.iter()) {
            mat_node
                .params
                .insert(format!("{prefix}_uv_{part}"), float(*value));
        }
        insert_sampler_params(
            mat_node,
            &format!("{prefix}_"),
            info.sampler,
            &format!("{prefix}_tex_coord"),
            info.tex_coord,
        );
    }
}

fn sampler_desc(info: GltfSamplerInfo) -> MapSamplerDesc {
    let filter = |value| match value {
        GltfFilterMode::Nearest => manifold_gpu::GpuFilterMode::Nearest,
        GltfFilterMode::Linear => manifold_gpu::GpuFilterMode::Linear,
    };
    MapSamplerDesc {
        wrap_u: match info.wrap_u {
            GltfWrapMode::Repeat => manifold_gpu::GpuAddressMode::Repeat,
            GltfWrapMode::ClampToEdge => manifold_gpu::GpuAddressMode::ClampToEdge,
            GltfWrapMode::MirrorRepeat => manifold_gpu::GpuAddressMode::MirrorRepeat,
        },
        wrap_v: match info.wrap_v {
            GltfWrapMode::Repeat => manifold_gpu::GpuAddressMode::Repeat,
            GltfWrapMode::ClampToEdge => manifold_gpu::GpuAddressMode::ClampToEdge,
            GltfWrapMode::MirrorRepeat => manifold_gpu::GpuAddressMode::MirrorRepeat,
        },
        mag_filter: filter(info.mag_filter),
        min_filter: filter(info.min_filter),
        mip_filter: info.mip_filter.map(filter),
    }
}

fn insert_sampler_params(
    mat_node: &mut EffectGraphNode,
    prefix: &str,
    sampler: MapSamplerDesc,
    tex_coord_name: &str,
    tex_coord: u32,
) {
    let wrap_idx = |mode| match mode {
        manifold_gpu::GpuAddressMode::Repeat => 0,
        manifold_gpu::GpuAddressMode::ClampToEdge => 1,
        manifold_gpu::GpuAddressMode::MirrorRepeat => 2,
        manifold_gpu::GpuAddressMode::ClampToZero => 1,
    };
    let filter_idx = |mode| match mode {
        manifold_gpu::GpuFilterMode::Linear => 0,
        manifold_gpu::GpuFilterMode::Nearest => 1,
    };
    mat_node.params.insert(
        format!("{prefix}wrap_u"),
        enum_val(wrap_idx(sampler.wrap_u)),
    );
    mat_node.params.insert(
        format!("{prefix}wrap_v"),
        enum_val(wrap_idx(sampler.wrap_v)),
    );
    mat_node.params.insert(
        format!("{prefix}mag_filter"),
        enum_val(filter_idx(sampler.mag_filter)),
    );
    mat_node.params.insert(
        format!("{prefix}min_filter"),
        enum_val(filter_idx(sampler.min_filter)),
    );
    mat_node.params.insert(
        format!("{prefix}mip_filter"),
        enum_val(match sampler.mip_filter {
            None => 2,
            Some(manifold_gpu::GpuFilterMode::Nearest) => 1,
            Some(manifold_gpu::GpuFilterMode::Linear) => 0,
        }),
    );
    mat_node
        .params
        .insert(tex_coord_name.to_string(), int(tex_coord as i32));
}
