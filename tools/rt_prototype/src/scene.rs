//! GLB load + merge-to-single-geometry, per BRIEF.md step 1.
//!
//! Simplification (P0 measurement scope, not product code): albedo textures
//! are baked to a per-material average color on the CPU rather than sampled
//! per-pixel in the G-buffer fragment shader. `docs/RAYTRACING_DESIGN.md` §5
//! P0 asks for numbers and images, not photorealistic texturing, and this
//! avoids a bindless/argument-buffer texture-array path that would add
//! real complexity for no measurement value here.

use glam::Vec3;

pub struct Material {
    pub albedo: [f32; 3],
    pub metallic: f32,
    pub roughness: f32,
    pub emissive: [f32; 3],
}

pub struct Scene {
    pub positions: Vec<[f32; 3]>,
    pub normals: Vec<[f32; 3]>,
    pub material_ids: Vec<u32>, // per-vertex, len == positions.len()
    pub indices: Vec<u32>,
    pub materials: Vec<Material>,
    pub center: Vec3,
    pub radius: f32,
}

fn average_texture_color(image: &gltf::image::Data) -> [f32; 3] {
    use gltf::image::Format;
    let (channels, srgb) = match image.format {
        Format::R8 => (1, false),
        Format::R8G8 => (2, false),
        Format::R8G8B8 => (3, true),
        Format::R8G8B8A8 => (4, true),
        Format::R16 => (1, false),
        Format::R16G16 => (2, false),
        Format::R16G16B16 => (3, false),
        Format::R16G16B16A16 => (4, false),
        Format::R32G32B32FLOAT => (3, false),
        Format::R32G32B32A32FLOAT => (4, false),
    };
    let px_count = image.pixels.len() / channels.max(1)
        / if matches!(image.format, Format::R16 | Format::R16G16 | Format::R16G16B16 | Format::R16G16B16A16) {
            2
        } else if matches!(image.format, Format::R32G32B32FLOAT | Format::R32G32B32A32FLOAT) {
            4
        } else {
            1
        };
    if px_count == 0 {
        return [1.0, 1.0, 1.0];
    }
    let mut sum = [0f64; 3];
    // Only the common 8-bit-per-channel formats are handled with real
    // averaging (what glTF exporters emit almost universally); anything
    // else falls back to white so material factor drives the color.
    if matches!(image.format, Format::R8G8B8 | Format::R8G8B8A8) {
        let stride = channels;
        for px in image.pixels.chunks_exact(stride) {
            for c in 0..3 {
                let v = px[c] as f64 / 255.0;
                sum[c] += if srgb { srgb_to_linear(v) } else { v };
            }
        }
        let n = px_count as f64;
        return [(sum[0] / n) as f32, (sum[1] / n) as f32, (sum[2] / n) as f32];
    }
    [1.0, 1.0, 1.0]
}

fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn load(path: &std::path::Path) -> Scene {
    let (document, buffers, images) =
        gltf::import(path).unwrap_or_else(|e| panic!("gltf import failed for {path:?}: {e}"));

    let mut materials = Vec::new();
    for m in document.materials() {
        let pbr = m.pbr_metallic_roughness();
        let mut albedo = pbr.base_color_factor();
        if let Some(tex) = pbr.base_color_texture() {
            let img = &images[tex.texture().source().index()];
            let avg = average_texture_color(img);
            albedo[0] *= avg[0];
            albedo[1] *= avg[1];
            albedo[2] *= avg[2];
        }
        materials.push(Material {
            albedo: [albedo[0], albedo[1], albedo[2]],
            metallic: pbr.metallic_factor(),
            roughness: pbr.roughness_factor(),
            emissive: m.emissive_factor(),
        });
    }
    if materials.is_empty() {
        materials.push(Material {
            albedo: [0.8, 0.8, 0.8],
            metallic: 0.0,
            roughness: 0.5,
            emissive: [0.0, 0.0, 0.0],
        });
    }

    let mut positions: Vec<[f32; 3]> = Vec::new();
    let mut normals: Vec<[f32; 3]> = Vec::new();
    let mut material_ids: Vec<u32> = Vec::new();
    let mut indices: Vec<u32> = Vec::new();
    let mut tri_count_per_material = vec![0u64; materials.len()];

    fn walk(
        node: &gltf::Node,
        parent: glam::Mat4,
        buffers: &[gltf::buffer::Data],
        materials_len: usize,
        positions: &mut Vec<[f32; 3]>,
        normals: &mut Vec<[f32; 3]>,
        material_ids: &mut Vec<u32>,
        indices: &mut Vec<u32>,
        tri_count_per_material: &mut [u64],
    ) {
        let local = glam::Mat4::from_cols_array_2d(&node.transform().matrix());
        let world = parent * local;
        let normal_mat = world.inverse().transpose();

        if let Some(mesh) = node.mesh() {
            for prim in mesh.primitives() {
                let reader = prim.reader(|b| Some(&buffers[b.index()]));
                let Some(pos_iter) = reader.read_positions() else {
                    continue;
                };
                let base_vertex = positions.len() as u32;
                let mat_id = prim
                    .material()
                    .index()
                    .map(|i| i as u32)
                    .unwrap_or_else(|| (materials_len - 1) as u32);

                let raw_positions: Vec<[f32; 3]> = pos_iter.collect();
                let raw_normals: Vec<[f32; 3]> = reader
                    .read_normals()
                    .map(|it| it.collect())
                    .unwrap_or_else(|| vec![[0.0, 1.0, 0.0]; raw_positions.len()]);

                for i in 0..raw_positions.len() {
                    let wp = world.transform_point3(Vec3::from(raw_positions[i]));
                    let wn = normal_mat
                        .transform_vector3(Vec3::from(raw_normals[i]))
                        .normalize_or_zero();
                    positions.push(wp.into());
                    normals.push(wn.into());
                    material_ids.push(mat_id);
                }

                let prim_indices: Vec<u32> = match reader.read_indices() {
                    Some(it) => it.into_u32().collect(),
                    None => (0..raw_positions.len() as u32).collect(),
                };
                for chunk in prim_indices.chunks_exact(3) {
                    indices.push(base_vertex + chunk[0]);
                    indices.push(base_vertex + chunk[1]);
                    indices.push(base_vertex + chunk[2]);
                    tri_count_per_material[mat_id as usize] += 1;
                }
            }
        }

        for child in node.children() {
            walk(
                &child,
                world,
                buffers,
                materials_len,
                positions,
                normals,
                material_ids,
                indices,
                tri_count_per_material,
            );
        }
    }

    let scene = document
        .default_scene()
        .unwrap_or_else(|| document.scenes().next().expect("GLB has no scenes"));
    for node in scene.nodes() {
        walk(
            &node,
            glam::Mat4::IDENTITY,
            &buffers,
            materials.len(),
            &mut positions,
            &mut normals,
            &mut material_ids,
            &mut indices,
            &mut tri_count_per_material,
        );
    }

    assert!(!positions.is_empty(), "GLB produced zero vertices");
    assert!(!indices.is_empty(), "GLB produced zero triangles");

    let nan_verts = positions.iter().filter(|p| p.iter().any(|c| !c.is_finite())).count();
    let degenerate_tris = indices
        .chunks_exact(3)
        .filter(|tri| {
            let a = Vec3::from(positions[tri[0] as usize]);
            let b = Vec3::from(positions[tri[1] as usize]);
            let c = Vec3::from(positions[tri[2] as usize]);
            (b - a).cross(c - a).length_squared() < 1e-20
        })
        .count();
    eprintln!("[scene] diagnostics: nan_or_inf_verts={nan_verts} degenerate_tris={degenerate_tris}");

    // Bounding sphere: box-center + half-diagonal radius (conservative, not
    // minimal — fine for framing a static hero-scan camera).
    let mut min = Vec3::from(positions[0]);
    let mut max = min;
    for p in &positions {
        let v = Vec3::from(*p);
        min = min.min(v);
        max = max.max(v);
    }
    let center = (min + max) * 0.5;
    let mut radius = (max - min).length() * 0.5;
    if radius <= 0.0 {
        radius = 1.0;
    }

    let has_emissive = materials.iter().any(|m| {
        m.emissive[0] > 1e-4 || m.emissive[1] > 1e-4 || m.emissive[2] > 1e-4
    });
    if !has_emissive
        && let Some((idx, _)) = tri_count_per_material
            .iter()
            .enumerate()
            .filter(|&(_, &c)| c > 0)
            .min_by_key(|&(_, &c)| c)
    {
        materials[idx].emissive = [6.0 * 20.0, 2.0 * 20.0, 1.0 * 20.0];
        println!(
            "[scene] no emissive material in GLB — forcing material {idx} \
             ({} tris) to emissive (120, 40, 20) so D4 gets exercised",
            tri_count_per_material[idx]
        );
    }

    println!(
        "[scene] {} verts, {} tris, {} materials, bounds center={:?} radius={:.3}",
        positions.len(),
        indices.len() / 3,
        materials.len(),
        center,
        radius
    );

    Scene {
        positions,
        normals,
        material_ids,
        indices,
        materials,
        center,
        radius,
    }
}
