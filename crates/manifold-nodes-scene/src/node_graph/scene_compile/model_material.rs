//! A material of a model file (SCENE_DOCUMENT_DESIGN.md section 7.7
//! (Imports)): the old importer's material wiring, moved here. Every glTF
//! factor and extension becomes a `node.pbr_material` param, and every map
//! one `node.gltf_texture_source` feeding the matching `node.scene_object`
//! port of each object drawing with the material.
//!
//! The eighteen map families are a catalog ([`MAP_FAMILIES`]), one row per
//! glTF texture map, differing only in which field carries the texture
//! index, the decode colour space, the port and (for the spec-gloss stand-in
//! alone) a channel repack mode. The base colour map is the explicit first
//! map. One source node serves every port that reads the same image with
//! the same decode.

use manifold_core::effect_graph_def::SerializedParamValue;
use manifold_node_engine::scene::material::MapSamplerDesc;

use super::builder::Builder;
use crate::node_graph::gltf_import::model_file::ModelFile;
use crate::node_graph::gltf_load::{GltfFilterMode, GltfMaterialInfo, GltfSamplerInfo, GltfWrapMode};

/// The surface params and texture maps of `file`'s material `material` on
/// `owner`'s surface node. Returns each map's source node and the
/// `node.scene_object` port it feeds.
pub(crate) fn emit(
    b: &mut Builder<'_>,
    owner: &str,
    surface: u32,
    file: &str,
    model: &ModelFile,
    material: Option<u32>,
) -> Vec<(u32, &'static str)> {
    let m = model.material(material);
    write_surface(b, surface, m);
    let mut maps: Vec<(u32, &'static str)> = Vec::new();
    // Keyed by (texture index, colour space, repack mode): an ORM-packed
    // file reads one image for occlusion and metal-roughness, while
    // specular and specular colour may read one image with two decodes.
    let mut sources: Vec<((u32, u32, u32), u32)> = Vec::new();
    for family in std::iter::once(&BASE_COLOR).chain(MAP_FAMILIES) {
        let Some(texture) = (family.texture)(m) else {
            continue;
        };
        let mode = (family.channel_mode)(m);
        let key = (texture, family.color_space, mode);
        let source = match sources.iter().find(|(k, _)| *k == key) {
            Some((_, source)) => *source,
            None => {
                let source = b.add(owner, family.port, "node.gltf_texture_source");
                b.set(source, "path", SerializedParamValue::String { value: file.to_owned() });
                b.set(source, "texture_index", SerializedParamValue::Int { value: texture as i32 });
                b.set(source, "color_space", SerializedParamValue::Enum { value: family.color_space });
                b.set(source, "mode", SerializedParamValue::Enum { value: mode });
                // Spec-gloss alpha stores glossiness. The source blit applies
                // the authored factor once: roughness = 1 - factor * alpha.
                let glossiness = if mode == 1 { (1.0 - m.roughness).clamp(0.0, 1.0) } else { 1.0 };
                b.set(source, "glossiness_factor", SerializedParamValue::Float { value: glossiness });
                // The image's own size; 8192 is the param's range limit.
                let (w, h) = model.texture_dims.get(texture as usize).copied().unwrap_or((1024, 1024));
                b.set(source, "width", SerializedParamValue::Int { value: w.min(8192) as i32 });
                b.set(source, "height", SerializedParamValue::Int { value: h.min(8192) as i32 });
                sources.push((key, source));
                source
            }
        };
        maps.push((source, family.port));
    }
    maps
}

/// The `node.scene_object` ports [`emit`] feeds for `model`'s material
/// `material`.
pub(super) fn map_ports(model: &ModelFile, material: Option<u32>) -> impl Iterator<Item = &'static str> + '_ {
    let m = model.material(material);
    std::iter::once(&BASE_COLOR).chain(MAP_FAMILIES).filter(move |family| (family.texture)(m).is_some()).map(|family| family.port)
}

/// The glTF factors, the computed params and every map's texture transform,
/// sampler and UV set, written onto the surface node.
fn write_surface(b: &mut Builder<'_>, surface: u32, m: &GltfMaterialInfo) {
    for param in MATERIAL_PARAMS {
        b.set_number(surface, param.name, (param.value)(m));
    }
    write_map_metadata(b, surface, m);
    // KHR_materials_transmission is real refraction in the shader, so its
    // see-through never darkens alpha; Blend and glass keep alpha as the
    // authored base colour alpha.
    let is_glass = m.was_blend || m.transmission_factor > 0.0;
    b.set_number(surface, "color_a", m.base_color_factor[3].clamp(0.0, 1.0));
    // A gloss map's conversion already applies the authored gloss factor.
    b.set_number(surface, "roughness", if m.mr_texture_is_gloss_alpha { 1.0 } else { m.roughness.max(0.01) });
    // KHR_materials_emissive_strength folds into the emission multiplier;
    // a material with no emissive colour stays dark.
    let emissive_lit = m.emissive.iter().any(|&c| c > 0.0);
    b.set_number(surface, "emission_intensity", if emissive_lit { m.emissive_strength } else { 0.0 });
    let alpha_mode = if is_glass {
        2 // Blend
    } else if m.alpha_mask {
        1 // Mask
    } else {
        0 // Opaque
    };
    b.set(surface, "alpha_mode", SerializedParamValue::Enum { value: alpha_mode });
}

/// One glTF texture-map family.
struct MapFamily {
    /// The field carrying this family's texture index; `None` when the
    /// material does not use the map.
    texture: fn(&GltfMaterialInfo) -> Option<u32>,
    /// 0 = sRGB (colour maps), 1 = Linear (data maps), per each family's
    /// KHR spec section.
    color_space: u32,
    /// The `node.scene_object` port, and the role of the source node made
    /// for it (`scene_schema::material::MODEL_MAP_ROLES`).
    port: &'static str,
    /// 1 repacks a spec-gloss alpha into G = roughness, B = metallic
    /// (GLB_XFAIL_BURNDOWN_DESIGN.md D2); 0 passes through.
    channel_mode: fn(&GltfMaterialInfo) -> u32,
}

const BASE_COLOR: MapFamily = MapFamily {
    texture: |m| m.base_color_texture,
    color_space: 0, // sRGB: albedo
    port: "base_color_map",
    channel_mode: |_| 0,
};

/// The eighteen map families, in wiring order.
const MAP_FAMILIES: &[MapFamily] = &[
    MapFamily { texture: |m| m.normal_texture, color_space: 1, port: "normal_map", channel_mode: |_| 0 },
    MapFamily {
        texture: |m| m.mr_texture,
        color_space: 1,
        port: "mr_map",
        channel_mode: |m| u32::from(m.mr_texture_is_gloss_alpha),
    },
    MapFamily { texture: |m| m.occlusion_texture, color_space: 1, port: "occlusion_map", channel_mode: |_| 0 },
    MapFamily { texture: |m| m.emissive_texture, color_space: 0, port: "emissive_map", channel_mode: |_| 0 },
    MapFamily { texture: |m| m.sheen_color_texture, color_space: 0, port: "sheen_color_map", channel_mode: |_| 0 },
    MapFamily {
        texture: |m| m.sheen_roughness_texture,
        color_space: 1,
        port: "sheen_roughness_map",
        channel_mode: |_| 0,
    },
    MapFamily { texture: |m| m.iridescence_texture, color_space: 1, port: "iridescence_map", channel_mode: |_| 0 },
    MapFamily {
        texture: |m| m.iridescence_thickness_texture,
        color_space: 1,
        port: "iridescence_thickness_map",
        channel_mode: |_| 0,
    },
    MapFamily { texture: |m| m.anisotropy_texture, color_space: 1, port: "anisotropy_map", channel_mode: |_| 0 },
    MapFamily { texture: |m| m.clearcoat_texture, color_space: 1, port: "clearcoat_map", channel_mode: |_| 0 },
    MapFamily {
        texture: |m| m.clearcoat_roughness_texture,
        color_space: 1,
        port: "clearcoat_roughness_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.clearcoat_normal_texture,
        color_space: 1,
        port: "clearcoat_normal_map",
        channel_mode: |_| 0,
    },
    MapFamily { texture: |m| m.specular_texture, color_space: 1, port: "specular_map", channel_mode: |_| 0 },
    MapFamily {
        texture: |m| m.specular_color_texture,
        color_space: 0,
        port: "specular_color_map",
        channel_mode: |_| 0,
    },
    MapFamily { texture: |m| m.transmission_texture, color_space: 1, port: "transmission_map", channel_mode: |_| 0 },
    MapFamily {
        texture: |m| m.diffuse_transmission_texture,
        color_space: 1,
        port: "diffuse_transmission_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.diffuse_transmission_color_texture,
        color_space: 0,
        port: "diffuse_transmission_color_map",
        channel_mode: |_| 0,
    },
    MapFamily {
        texture: |m| m.volume_thickness_texture,
        color_space: 1,
        port: "volume_thickness_map",
        channel_mode: |_| 0,
    },
];

/// One plain glTF field written to a `node.pbr_material` param.
struct MaterialParam {
    name: &'static str,
    value: fn(&GltfMaterialInfo) -> f32,
}

/// The plain field to param catalog. Extension defaults reproduce glTF's own
/// (ior 1.5, specular 1.0, clearcoat 0.0, ...), so a material without an
/// extension writes the same params as one that never had it.
const MATERIAL_PARAMS: &[MaterialParam] = &[
    MaterialParam { name: "color_r", value: |m| m.base_color_factor[0] },
    MaterialParam { name: "color_g", value: |m| m.base_color_factor[1] },
    MaterialParam { name: "color_b", value: |m| m.base_color_factor[2] },
    MaterialParam { name: "metallic", value: |m| m.metallic },
    MaterialParam { name: "normal_scale", value: |m| m.normal_scale },
    MaterialParam { name: "clearcoat_normal_scale", value: |m| m.clearcoat_normal_scale },
    MaterialParam { name: "occlusion_strength", value: |m| m.occlusion_strength },
    MaterialParam { name: "emission_r", value: |m| m.emissive[0] },
    MaterialParam { name: "emission_g", value: |m| m.emissive[1] },
    MaterialParam { name: "emission_b", value: |m| m.emissive[2] },
    MaterialParam { name: "alpha_cutoff", value: |m| m.alpha_cutoff },
    MaterialParam { name: "ior", value: |m| m.ior },
    MaterialParam { name: "specular", value: |m| m.specular_factor },
    MaterialParam { name: "specular_tint_r", value: |m| m.specular_color_factor[0] },
    MaterialParam { name: "specular_tint_g", value: |m| m.specular_color_factor[1] },
    MaterialParam { name: "specular_tint_b", value: |m| m.specular_color_factor[2] },
    MaterialParam { name: "clearcoat", value: |m| m.clearcoat_factor },
    MaterialParam { name: "clearcoat_roughness", value: |m| m.clearcoat_roughness_factor },
    MaterialParam { name: "sheen_color_r", value: |m| m.sheen_color_factor[0] },
    MaterialParam { name: "sheen_color_g", value: |m| m.sheen_color_factor[1] },
    MaterialParam { name: "sheen_color_b", value: |m| m.sheen_color_factor[2] },
    MaterialParam { name: "sheen_roughness", value: |m| m.sheen_roughness_factor },
    MaterialParam { name: "iridescence", value: |m| m.iridescence_factor },
    MaterialParam { name: "iridescence_ior", value: |m| m.iridescence_ior },
    MaterialParam { name: "iridescence_thickness_min", value: |m| m.iridescence_thickness_minimum },
    MaterialParam { name: "iridescence_thickness_max", value: |m| m.iridescence_thickness_maximum },
    MaterialParam { name: "anisotropy_strength", value: |m| m.anisotropy_strength },
    MaterialParam { name: "anisotropy_rotation", value: |m| m.anisotropy_rotation },
    MaterialParam { name: "dispersion", value: |m| m.dispersion },
    MaterialParam { name: "transmission", value: |m| m.transmission_factor },
    MaterialParam { name: "volume_thickness", value: |m| m.volume_thickness_factor },
    MaterialParam { name: "volume_attenuation_distance", value: |m| m.volume_attenuation_distance },
    MaterialParam { name: "volume_attenuation_color_r", value: |m| m.volume_attenuation_color[0] },
    MaterialParam { name: "volume_attenuation_color_g", value: |m| m.volume_attenuation_color[1] },
    MaterialParam { name: "volume_attenuation_color_b", value: |m| m.volume_attenuation_color[2] },
    // KHR_materials_diffuse_transmission drives the translucency params
    // (RAYTRACING_DESIGN.md section 16 (RT translucency), decision TL3).
    MaterialParam { name: "translucency", value: |m| m.diffuse_transmission_factor },
    MaterialParam { name: "translucency_color_r", value: |m| m.diffuse_transmission_color[0] },
    MaterialParam { name: "translucency_color_g", value: |m| m.diffuse_transmission_color[1] },
    MaterialParam { name: "translucency_color_b", value: |m| m.diffuse_transmission_color[2] },
];

/// The six components of a `KHR_texture_transform` affine, in param order.
const UV_TRANSFORM_PARTS: [&str; 6] = ["m00", "m01", "m10", "m11", "tx", "ty"];

/// The extension maps, in `GltfMaterialInfo::extension_maps` order.
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

/// Every map's texture transform, sampler and UV set.
fn write_map_metadata(b: &mut Builder<'_>, surface: u32, m: &GltfMaterialInfo) {
    let core = [
        ("uv_", "", "uv_set", &m.base_color_uv_transform, m.core_tex_coords[0], sampler_desc(m.base_color_sampler)),
        ("nrm_uv_", "nrm_", "nrm_uv_set", &m.normal_uv_transform, m.core_tex_coords[1], sampler_desc(m.normal_sampler)),
        ("mr_uv_", "mr_", "mr_uv_set", &m.mr_uv_transform, m.core_tex_coords[2], sampler_desc(m.mr_sampler)),
        ("occ_uv_", "occ_", "occ_uv_set", &m.occlusion_uv_transform, m.core_tex_coords[3], sampler_desc(m.occlusion_sampler)),
        ("em_uv_", "em_", "em_uv_set", &m.emissive_uv_transform, m.core_tex_coords[4], sampler_desc(m.emissive_sampler)),
    ];
    for (uv_prefix, sampler_prefix, tex_coord_name, transform, tex_coord, sampler) in core {
        for (part, value) in UV_TRANSFORM_PARTS.iter().zip(transform.iter()) {
            b.set_number(surface, &format!("{uv_prefix}{part}"), *value);
        }
        sampler_params(b, surface, sampler_prefix, sampler, tex_coord_name, tex_coord);
    }
    for (prefix, info) in EXTENSION_MAP_PREFIXES.iter().zip(m.extension_maps.iter()) {
        for (part, value) in UV_TRANSFORM_PARTS.iter().zip(info.uv_transform.iter()) {
            b.set_number(surface, &format!("{prefix}_uv_{part}"), *value);
        }
        sampler_params(b, surface, &format!("{prefix}_"), info.sampler, &format!("{prefix}_tex_coord"), info.tex_coord);
    }
}

fn sampler_desc(info: GltfSamplerInfo) -> MapSamplerDesc {
    let filter = |value| match value {
        GltfFilterMode::Nearest => manifold_gpu::GpuFilterMode::Nearest,
        GltfFilterMode::Linear => manifold_gpu::GpuFilterMode::Linear,
    };
    let wrap = |value| match value {
        GltfWrapMode::Repeat => manifold_gpu::GpuAddressMode::Repeat,
        GltfWrapMode::ClampToEdge => manifold_gpu::GpuAddressMode::ClampToEdge,
        GltfWrapMode::MirrorRepeat => manifold_gpu::GpuAddressMode::MirrorRepeat,
    };
    MapSamplerDesc {
        wrap_u: wrap(info.wrap_u),
        wrap_v: wrap(info.wrap_v),
        mag_filter: filter(info.mag_filter),
        min_filter: filter(info.min_filter),
        mip_filter: info.mip_filter.map(filter),
    }
}

fn sampler_params(
    b: &mut Builder<'_>,
    surface: u32,
    prefix: &str,
    sampler: MapSamplerDesc,
    tex_coord_name: &str,
    tex_coord: u32,
) {
    let wrap = |mode| match mode {
        manifold_gpu::GpuAddressMode::Repeat => 0,
        manifold_gpu::GpuAddressMode::ClampToEdge | manifold_gpu::GpuAddressMode::ClampToZero => 1,
        manifold_gpu::GpuAddressMode::MirrorRepeat => 2,
    };
    let filter = |mode| match mode {
        manifold_gpu::GpuFilterMode::Linear => 0,
        manifold_gpu::GpuFilterMode::Nearest => 1,
    };
    let mip = match sampler.mip_filter {
        None => 2,
        Some(manifold_gpu::GpuFilterMode::Nearest) => 1,
        Some(manifold_gpu::GpuFilterMode::Linear) => 0,
    };
    for (name, value) in [
        ("wrap_u", wrap(sampler.wrap_u)),
        ("wrap_v", wrap(sampler.wrap_v)),
        ("mag_filter", filter(sampler.mag_filter)),
        ("min_filter", filter(sampler.min_filter)),
        ("mip_filter", mip),
    ] {
        b.set(surface, &format!("{prefix}{name}"), SerializedParamValue::Enum { value });
    }
    b.set(surface, tex_coord_name, SerializedParamValue::Int { value: tex_coord as i32 });
}
