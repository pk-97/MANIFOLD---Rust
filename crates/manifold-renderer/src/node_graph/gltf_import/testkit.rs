/// A synthetic [`GltfMaterialInfo`] carrying every texture kind F-P4
/// wires, with independent test-controlled fields for the three
/// report-only features (clearcoat/transmission/BLEND). Defaults mirror
/// a "fully-mapped, nothing extra" material — callers override only
/// what a specific test cares about (Rust has no field-update syntax
/// across `..` for `pub(crate)` structs outside the defining module, so
/// this is a plain builder-by-closure, not `..Default::default()`).
pub fn full_material(material_index: u32, name: &str, verts: u32) -> super::gltf_load::GltfMaterialInfo {
    use super::gltf_load::GltfMaterialInfo;
    GltfMaterialInfo {
        material_index,
        name: Some(name.to_string()),
        base_color_factor: [0.8, 0.8, 0.8, 1.0],
        metallic: 1.0,
        roughness: 0.4,
        emissive: [1.0, 0.5, 0.2],
        alpha_mask: false,
        alpha_cutoff: 0.5,
        base_color_texture: Some(0),
        normal_texture: Some(1),
        normal_scale: 1.0,
        mr_texture: Some(2),
        occlusion_texture: Some(3),
        occlusion_strength: 1.0,
        emissive_texture: Some(4),
        emissive_strength: 2.5,
        ior: 1.5,
        specular_factor: 1.0,
        legacy_specular_factor: None,
        specular_color_factor: [1.0, 1.0, 1.0],
        specular_texture: None,
        specular_color_texture: None,
        base_color_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        normal_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        mr_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        occlusion_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        emissive_uv_transform: super::gltf_load::IDENTITY_UV_TRANSFORM,
        core_tex_coords: [0; 5],
        mr_texture_is_gloss_alpha: false,
        transmission_factor: 0.0,
        transmission_texture: None,
        diffuse_transmission_factor: 0.0,
        diffuse_transmission_color: [1.0, 1.0, 1.0],
        diffuse_transmission_texture: None,
        diffuse_transmission_color_texture: None,
        clearcoat_factor: 0.0,
        clearcoat_roughness_factor: 0.0,
        clearcoat_normal_scale: 1.0,
        clearcoat_texture: None,
        clearcoat_roughness_texture: None,
        clearcoat_normal_texture: None,
        sheen_color_factor: [0.0, 0.0, 0.0],
        sheen_roughness_factor: 0.0,
        sheen_color_texture: None,
        sheen_roughness_texture: None,
        iridescence_factor: 0.0,
        iridescence_ior: 1.3,
        iridescence_thickness_minimum: 100.0,
        iridescence_thickness_maximum: 400.0,
        iridescence_texture: None,
        iridescence_thickness_texture: None,
        anisotropy_strength: 0.0,
        anisotropy_rotation: 0.0,
        anisotropy_texture: None,
        dispersion: 0.0,
        volume_thickness_factor: 0.0,
        volume_attenuation_distance: manifold_node_engine::scene::material::VOLUME_ATTENUATION_DISTANCE_NO_ATTENUATION,
        volume_attenuation_color: [1.0, 1.0, 1.0],
        volume_thickness_texture: None,
        was_blend: false,
        vertex_color_varies: false,
        unlit: false,
        vertex_count: verts,
        base_color_sampler: super::gltf_load::GltfSamplerInfo::default(),
        normal_sampler: super::gltf_load::GltfSamplerInfo::default(),
        mr_sampler: super::gltf_load::GltfSamplerInfo::default(),
        occlusion_sampler: super::gltf_load::GltfSamplerInfo::default(),
        emissive_sampler: super::gltf_load::GltfSamplerInfo::default(),
        extension_maps: [manifold_node_engine::scene::material::MaterialMapInfo::default(); 14],
        animations: Vec::new(),
        skin: None,
        morph: None,
        rigid_multi_node: None,
        own_center: [0.0, 0.0, 0.0],
    }
}

/// Build a minimal, valid `.glb` with `n` distinct materials, each owning
/// exactly one triangle (so every material has geometry and therefore
/// counts toward `ImportReport::material_count`) — hand-rolled binary
/// container (12-byte header + JSON chunk + BIN chunk, no external
/// `.bin`/textures, no `uri` on the buffer so it resolves to the BIN
/// chunk per spec section Binary glTF). GLB_CONFORMANCE_DESIGN.md G-P2: proves
/// the FULL production parse path (`gltf::import` → `gltf_import_summary`
/// → `build_import_graph`) imports every material 1:1, not just the
/// graph-assembly half a synthetic [`GltfImportSummary`] would exercise.
/// Written to the OS temp dir, not committed — a builder fn, not a
/// binary asset (the phase brief's explicit call).
pub fn write_synthetic_multimaterial_glb(n: usize) -> std::path::PathBuf {
    let mut accessors = Vec::with_capacity(n);
    let mut buffer_views = Vec::with_capacity(n);
    let mut materials = Vec::with_capacity(n);
    let mut primitives = Vec::with_capacity(n);
    let mut bin = Vec::with_capacity(n * 36);

    for i in 0..n {
        // One triangle per material, spread along X so no two overlap —
        // cosmetic, but keeps bbox/normal math non-degenerate.
        let ox = i as f32 * 2.0;
        let tri: [[f32; 3]; 3] = [[ox, 0.0, 0.0], [ox + 1.0, 0.0, 0.0], [ox, 1.0, 0.0]];
        for v in &tri {
            for c in v {
                bin.extend_from_slice(&c.to_le_bytes());
            }
        }
        let byte_offset = i * 36;
        buffer_views.push(serde_json::json!({
            "buffer": 0,
            "byteOffset": byte_offset,
            "byteLength": 36,
        }));
        accessors.push(serde_json::json!({
            "bufferView": i,
            "componentType": 5126, // FLOAT
            "count": 3,
            "type": "VEC3",
            "min": [ox, 0.0, 0.0],
            "max": [ox + 1.0, 1.0, 0.0],
        }));
        materials.push(serde_json::json!({
            "name": format!("Mat{i}"),
            "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] },
        }));
        // Mode omitted — glTF's default primitive mode is 4 (TRIANGLES).
        primitives.push(serde_json::json!({
            "attributes": { "POSITION": i },
            "material": i,
        }));
    }

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": primitives }],
        "accessors": accessors,
        "bufferViews": buffer_views,
        "materials": materials,
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");

    // GLB container: header + JSON chunk (space-padded to 4 bytes) + BIN
    // chunk (zero-padded to 4 bytes). Chunk type magics per the Binary
    // glTF spec: 0x4E4F534A = "JSON", 0x004E4942 = "BIN\0".
    let mut json_padded = json_bytes;
    while !json_padded.len().is_multiple_of(4) {
        json_padded.push(b' ');
    }
    let mut bin_padded = bin;
    while !bin_padded.len().is_multiple_of(4) {
        bin_padded.push(0);
    }
    let total_len = 12 + 8 + json_padded.len() + 8 + bin_padded.len();

    let mut glb = Vec::with_capacity(total_len);
    glb.extend_from_slice(b"glTF");
    glb.extend_from_slice(&2u32.to_le_bytes());
    glb.extend_from_slice(&(total_len as u32).to_le_bytes());
    glb.extend_from_slice(&(json_padded.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"JSON");
    glb.extend_from_slice(&json_padded);
    glb.extend_from_slice(&(bin_padded.len() as u32).to_le_bytes());
    glb.extend_from_slice(b"BIN\0");
    glb.extend_from_slice(&bin_padded);

    let path = std::env::temp_dir().join(format!(
        "manifold_synthetic_{n}mat_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
    path
}

