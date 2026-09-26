//! glTF material factor serialization and map sampler metadata.
//!
//! This child module keeps the parameter catalog and per-map metadata
//! separate from texture-source wiring while preserving the parent
//! `materials::write_material_params` API.

use manifold_core::effect_graph_def::{EffectGraphNode, SerializedParamValue};

use crate::node_graph::gltf_load::{
    GltfFilterMode, GltfMaterialInfo, GltfSamplerInfo, GltfWrapMode,
};
use crate::node_graph::material::MapSamplerDesc;

use super::super::assembly::{enum_val, float, int};

/// The six components of a `KHR_texture_transform` affine, in the order the
/// per-map UV-transform params are named (`{prefix}m00` … `{prefix}ty`). Used
/// by the per-map UV-transform loop in `object_group.rs`, which pairs each
/// component name with the matching value off a borrowed `&m.<map>_uv_transform`
/// (so the loop's `(prefix, transform)` pairs cannot themselves be const).
pub(in crate::node_graph::gltf_import) const UV_TRANSFORM_PARTS: [&str; 6] = ["m00", "m01", "m10", "m11", "tx", "ty"];

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
pub(in crate::node_graph::gltf_import) fn write_material_params(mat_node: &mut EffectGraphNode, m: &GltfMaterialInfo) {
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
