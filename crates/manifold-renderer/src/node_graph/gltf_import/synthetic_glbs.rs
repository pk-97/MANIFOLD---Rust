//! Synthetic `.glb` fixture builders for the import test suite — split out of
//! `tests.rs` when that file crossed its 7000-line godfile ceiling
//! (BUG-adpu names this split). Each builder hand-rolls the GLB container
//! (header + space-padded JSON chunk + zero-padded BIN chunk) for one
//! hostile/edge-case asset shape. Self-contained: serde_json + std only.

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
pub(super) fn write_synthetic_multimaterial_glb(n: usize) -> std::path::PathBuf {
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
/// Build a one-triangle `.glb` spanning `[-half, +half]` on X/Y — the
/// smallest asset whose bbox exercises the synthesized camera's
/// size-scaled clip planes. Same container shape as
/// `write_synthetic_multimaterial_glb`.
pub(super) fn write_synthetic_sized_glb(half: f32) -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[-half, -half, 0.0], [half, -half, 0.0], [-half, half, 0.0]];
    let mut bin = Vec::with_capacity(36);
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 }, "material": 0 }] }],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [-half, -half, 0.0], "max": [half, half, 0.0],
        }],
        "bufferViews": [{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }],
        "materials": [{ "name": "Mat0", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } }],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let mut json_padded = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");
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
        "manifold_synthetic_sized_{}_{}_{}.glb",
        half,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
    path
}
/// Build a minimal, valid `.glb` with TWO materials: `Mat0` has one real
/// triangle (so the asset has SOME geometry and `gltf_import_summary`
/// doesn't bail with "parsed no geometry"), `Mat1`'s sole primitive is
/// tagged `KHR_draco_mesh_compression`.
///
/// A "no-fallback" Draco export (the common case — the whole point of
/// Draco is the size win, and a redundant uncompressed fallback accessor
/// gives most of that back) omits `bufferView` on the base POSITION
/// accessor entirely (spec-legal per KHR_draco_mesh_compression's
/// conformance text: Draco-aware loaders ignore that accessor's
/// `bufferView`/`byteOffset` anyway). That shape does NOT reach this
/// module at all today — `parse_document_and_buffers` re-runs the vendored
/// `gltf-json` crate's own structural `Validate` (BUG-213's filter only
/// drops the `extensionsRequired` check), and that crate's accessor
/// validation hook unconditionally requires `bufferView` unless the
/// accessor is `sparse` (`gltf-json-1.4.1/src/accessor.rs`
/// `accessor_validate_hook`) — it has no KHR_draco_mesh_compression
/// awareness, so a genuinely spec-legal no-fallback Draco asset fails the
/// WHOLE document's import at that gate with an opaque "bufferView
/// Missing" error, never reaching `summarize_node`'s primitive loop this
/// fix targets. Reported up alongside this change (see the lane report) —
/// out of this fix's scope, since relaxing that structural gate is a
/// separate, bigger decision.
///
/// This fixture instead reproduces the one Draco shape that DOES reach
/// `summarize_node` today without tripping that earlier gate: `Mat1`'s
/// POSITION accessor has a real `bufferView` (satisfies `Validate`) that
/// is deliberately too small for its declared `count`/`type` — the same
/// `reader.read_positions() == None` outcome (`accessor::Iter::new`'s
/// `slice.get(start..end)` fails on a too-short buffer view), reachable
/// via a truncated/corrupt buffer on ANY primitive, Draco-tagged or not.
/// Same hand-rolled binary-container shape as
/// `write_synthetic_multimaterial_glb`.
pub(super) fn write_synthetic_draco_glb() -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut bin = Vec::with_capacity(36);
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    // A few extra bytes so bufferView 1 (below) is a real, in-range slice
    // of the buffer — just too short for accessor 1's VEC3*3 = 36 bytes.
    bin.extend_from_slice(&[0u8; 4]);

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "extensionsUsed": ["KHR_draco_mesh_compression"],
        "scene": 0,
        "scenes": [{ "nodes": [0, 1] }],
        "nodes": [{ "mesh": 0 }, { "mesh": 1, "name": "DracoNode" }],
        "meshes": [
            { "primitives": [{ "attributes": { "POSITION": 0 }, "material": 0 }] },
            {
                "name": "DracoMesh",
                "primitives": [{
                    "attributes": { "POSITION": 1 },
                    "material": 1,
                    "extensions": {
                        "KHR_draco_mesh_compression": {
                            "bufferView": 0,
                            "attributes": { "POSITION": 0 }
                        }
                    }
                }]
            },
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
            // Structurally valid (has bufferView + min/max, passes
            // `Validate`) but the referenced bufferView is too short to
            // actually supply 3 VEC3<f32> — `read_positions()` returns
            // `None` at read time, same outcome a no-fallback Draco
            // accessor's missing `bufferView` would produce if it ever
            // got past the validation gate above.
            {
                "bufferView": 1,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
            { "buffer": 0, "byteOffset": 36, "byteLength": 4 },
        ],
        "materials": [
            { "name": "Mat0", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } },
            { "name": "Mat1", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } },
        ],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic draco glTF JSON");

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
        "manifold_synthetic_draco_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic draco glb to temp dir");
    path
}
/// Build a minimal, valid `.glb` with ONE material (`Mat0`, real triangle
/// geometry, so the asset always has something to import) whose
/// `baseColorTexture` points at a KTX2/BasisU image: `mimeType:
/// "image/ktx2"`, bufferView-embedded, and the bytes are garbage (not a
/// real KTX2 container) — MANIFOLD has no BasisU transcoder either way, so
/// the mime type alone is enough to prove the "unsupported source, not a
/// corrupt one" report line without needing a real KTX2 encoder in the test.
/// BUG-ssgz: this used to hard-fail the WHOLE document at `import_glb`
/// (`gltf::image::Data::from_source` errors on any unrecognized mime type,
/// same class of bug W1 already fixed for `image/webp`) — the mesh and
/// material must now still import, with the texture swapped for a dummy.
pub(super) fn write_synthetic_ktx2_glb() -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut bin = Vec::with_capacity(48);
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    // Not a real KTX2 container — just bytes no decoder anywhere
    // recognizes. The mime type, not the payload, is what makes this
    // KTX2/BasisU (a well-formed KTX2 file would fail to decode exactly
    // the same way — MANIFOLD's `image` crate has no BasisU transcoder,
    // see `crates/manifold-renderer/Cargo.toml`'s `image` feature list).
    let ktx2_bytes: [u8; 8] = [0xAB, 0x4B, 0x54, 0x58, 0x20, 0x32, 0x30, 0xBB];
    bin.extend_from_slice(&ktx2_bytes);

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [
            { "primitives": [{ "attributes": { "POSITION": 0 }, "material": 0 }] },
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
            { "buffer": 0, "byteOffset": 36, "byteLength": ktx2_bytes.len() },
        ],
        "images": [
            { "name": "BrokenKtx2", "bufferView": 1, "mimeType": "image/ktx2" },
        ],
        "textures": [
            { "source": 0 },
        ],
        "materials": [
            {
                "name": "Mat0",
                "pbrMetallicRoughness": {
                    "baseColorFactor": [0.5, 0.5, 0.5, 1.0],
                    "baseColorTexture": { "index": 0 },
                },
            },
        ],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic ktx2 glTF JSON");

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
        "manifold_synthetic_ktx2_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic ktx2 glb to temp dir");
    path
}
/// Build a minimal, valid `.glb`: one triangle whose primitive carries
/// BOTH `TEXCOORD_0` and `TEXCOORD_1`, textured by `Mat0` whose
/// `occlusionTexture` explicitly points at `texCoord: 1`. Exercises both
/// BUG-pm9m warning paths in one fixture — the material-slot check
/// (occlusion references TEXCOORD_1) and the primitive-attribute check
/// (the mesh itself carries a TEXCOORD_1 set) — same hand-rolled binary
/// container shape as `write_synthetic_ktx2_glb`, with a second UV
/// accessor and a `texCoord` override on the occlusion texture reference.
pub(super) fn write_synthetic_multi_uv_glb() -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let uv0: [[f32; 2]; 3] = [[0.0, 0.0], [1.0, 0.0], [0.0, 1.0]];
    let uv1: [[f32; 2]; 3] = [[0.0, 0.5], [1.0, 0.5], [0.0, 1.5]];

    let mut png = Vec::new();
    {
        use image::ImageEncoder;
        let pixel: [u8; 4] = [255, 255, 255, 255];
        image::codecs::png::PngEncoder::new(&mut png)
            .write_image(&pixel, 1, 1, image::ExtendedColorType::Rgba8)
            .expect("encode 1x1 fixture png");
    }

    let mut bin: Vec<u8> = Vec::new();
    let pos_off = bin.len();
    bin.extend(tri.iter().flatten().flat_map(|f| f.to_le_bytes()));
    let uv0_off = bin.len();
    bin.extend(uv0.iter().flatten().flat_map(|f| f.to_le_bytes()));
    let uv1_off = bin.len();
    bin.extend(uv1.iter().flatten().flat_map(|f| f.to_le_bytes()));
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }
    let png_off = bin.len();
    bin.extend_from_slice(&png);
    while !bin.len().is_multiple_of(4) {
        bin.push(0);
    }

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        "meshes": [
            {
                "primitives": [{
                    "attributes": { "POSITION": 0, "TEXCOORD_0": 1, "TEXCOORD_1": 2 },
                    "material": 0,
                }],
            },
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
            { "bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC2" },
            { "bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC2" },
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": pos_off, "byteLength": 36 },
            { "buffer": 0, "byteOffset": uv0_off, "byteLength": 24 },
            { "buffer": 0, "byteOffset": uv1_off, "byteLength": 24 },
            { "buffer": 0, "byteOffset": png_off, "byteLength": png.len() },
        ],
        "images": [
            { "name": "Occ", "bufferView": 3, "mimeType": "image/png" },
        ],
        "textures": [
            { "source": 0 },
        ],
        "materials": [
            {
                "name": "Mat0",
                "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] },
                "occlusionTexture": { "index": 0, "texCoord": 1 },
            },
        ],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic multi-UV glTF JSON");

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
        "manifold_synthetic_multiuv_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic multi-uv glb to temp dir");
    path
}
/// Build a minimal, valid `.glb` with TWO materials: `Mat0` has one real
/// triangle, `Mat1`'s sole primitive's POSITION accessor points at a
/// bufferView tagged `EXT_meshopt_compression`.
///
/// Unlike Draco (`write_synthetic_draco_glb`), meshopt keeps a normal,
/// correctly-sized bufferView — the whole point of the bug (BUG-7w79) is
/// that `reader.read_positions()` would happily succeed and hand back
/// compressed bytes reinterpreted as raw f32, silent garbage geometry with
/// no error. So `bufferView` 1 here is real, in-range, and exactly the
/// right length for 3 VEC3<f32> (unlike the Draco fixture's deliberately
/// truncated one) — detection must come from the `EXT_meshopt_compression`
/// tag in `bufferViews[1].extensions`, not from a failed read. No
/// `extensionsRequired` entry (same choice `write_synthetic_draco_glb`
/// makes for Draco): this fixture reproduces the "primitive silently reads
/// wrong" shape, not the separate "whole document refuses to load" gate.
pub(super) fn write_synthetic_meshopt_glb() -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut bin = Vec::with_capacity(72);
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    // Second bufferView's bytes are irrelevant content-wise (never read —
    // the fix must reject before the read) but must be a real, in-range,
    // correctly-sized slice so this fixture isolates the extension check
    // from the "too-short buffer view" path the Draco fixture exercises.
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "extensionsUsed": ["EXT_meshopt_compression"],
        "scene": 0,
        "scenes": [{ "nodes": [0, 1] }],
        "nodes": [{ "mesh": 0 }, { "mesh": 1, "name": "MeshoptNode" }],
        "meshes": [
            { "primitives": [{ "attributes": { "POSITION": 0 }, "material": 0 }] },
            {
                "name": "MeshoptMesh",
                "primitives": [{ "attributes": { "POSITION": 1 }, "material": 1 }]
            },
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
            {
                "bufferView": 1,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
            {
                "buffer": 0,
                "byteOffset": 36,
                "byteLength": 36,
                "extensions": {
                    "EXT_meshopt_compression": {
                        "buffer": 0,
                        "byteOffset": 36,
                        "byteLength": 36,
                        "byteStride": 12,
                        "count": 3,
                        "mode": "ATTRIBUTES",
                    },
                },
            },
        ],
        "materials": [
            { "name": "Mat0", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } },
            { "name": "Mat1", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } },
        ],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic meshopt glTF JSON");

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
        "manifold_synthetic_meshopt_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic meshopt glb to temp dir");
    path
}
/// Build a minimal, valid `.glb` with TWO materials: `Mat0` has one real
/// F32 triangle, `Mat1`'s sole primitive's POSITION accessor is
/// `KHR_mesh_quantization`-style: normalized SHORT components instead of
/// F32.
///
/// BUG-jfe2: the vendored gltf crate's `Item::from_slice` reinterprets
/// every accessor read as F32 stride — a normalized SHORT/BYTE accessor
/// misaligns every subsequent byte, silent garbage geometry with no error.
/// `bufferView` 1 here is real, in-range, and exactly the right length for
/// 3 VEC3<i16> (18 bytes) — detection must come from the accessor's
/// `componentType`, not from a failed read. No `extensionsRequired` entry
/// (same choice the Draco/meshopt fixtures make): this fixture reproduces
/// the "primitive silently reads wrong" shape, not the separate "whole
/// document refuses to load" gate.
pub(super) fn write_synthetic_quantized_position_glb() -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut bin = Vec::with_capacity(36 + 18);
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    // Normalized SHORT POSITION: 3 vertices * VEC3<i16> = 18 bytes.
    // Component values are arbitrary — never legitimately read as
    // dequantized geometry, only checked for componentType.
    let quantized_tri: [[i16; 3]; 3] = [[0, 0, 0], [32767, 0, 0], [0, 32767, 0]];
    for v in &quantized_tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "extensionsUsed": ["KHR_mesh_quantization"],
        "scene": 0,
        "scenes": [{ "nodes": [0, 1] }],
        "nodes": [{ "mesh": 0 }, { "mesh": 1, "name": "QuantizedPositionNode" }],
        "meshes": [
            { "primitives": [{ "attributes": { "POSITION": 0 }, "material": 0 }] },
            {
                "name": "QuantizedPositionMesh",
                "primitives": [{ "attributes": { "POSITION": 1 }, "material": 1 }]
            },
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
            {
                "bufferView": 1,
                "componentType": 5122, // SHORT
                "normalized": true,
                "count": 3,
                "type": "VEC3",
                "min": [0, 0, 0],
                "max": [32767, 32767, 0],
            },
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
            { "buffer": 0, "byteOffset": 36, "byteLength": 18 },
        ],
        "materials": [
            { "name": "Mat0", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } },
            { "name": "Mat1", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } },
        ],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic quantized-position glTF JSON");

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
        "manifold_synthetic_quantized_position_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic quantized-position glb to temp dir");
    path
}
/// Build a minimal, valid `.glb` with TWO materials: `Mat0` has one real
/// F32 triangle (no NORMAL, exercises the face-normal fallback path
/// elsewhere), `Mat1`'s sole primitive has a valid F32 POSITION accessor
/// but a `KHR_mesh_quantization`-style normalized BYTE NORMAL accessor.
///
/// Proves a quantized NORMAL (or TEXCOORD_0) drops the whole primitive
/// even when POSITION is valid F32 — partial garbage attributes (correct
/// positions, garbage normals) are not acceptable.
pub(super) fn write_synthetic_quantized_normal_glb() -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut bin = Vec::with_capacity(36 + 36 + 9);
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    // Mat1's valid F32 POSITION.
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }
    // Normalized BYTE NORMAL: 3 vertices * VEC3<i8> = 9 bytes. Component
    // values are arbitrary — never legitimately read, only checked for
    // componentType.
    let quantized_normals: [[i8; 3]; 3] = [[0, 0, 127], [0, 0, 127], [0, 0, 127]];
    for v in &quantized_normals {
        for c in v {
            bin.push(*c as u8);
        }
    }

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "extensionsUsed": ["KHR_mesh_quantization"],
        "scene": 0,
        "scenes": [{ "nodes": [0, 1] }],
        "nodes": [{ "mesh": 0 }, { "mesh": 1, "name": "QuantizedNormalNode" }],
        "meshes": [
            { "primitives": [{ "attributes": { "POSITION": 0 }, "material": 0 }] },
            {
                "name": "QuantizedNormalMesh",
                "primitives": [{ "attributes": { "POSITION": 1, "NORMAL": 2 }, "material": 1 }]
            },
        ],
        "accessors": [
            {
                "bufferView": 0,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
            {
                "bufferView": 1,
                "componentType": 5126, // FLOAT
                "count": 3,
                "type": "VEC3",
                "min": [0.0, 0.0, 0.0],
                "max": [1.0, 1.0, 0.0],
            },
            {
                "bufferView": 2,
                "componentType": 5120, // BYTE
                "normalized": true,
                "count": 3,
                "type": "VEC3",
                "min": [0, 0, 0],
                "max": [0, 0, 127],
            },
        ],
        "bufferViews": [
            { "buffer": 0, "byteOffset": 0, "byteLength": 36 },
            { "buffer": 0, "byteOffset": 36, "byteLength": 36 },
            { "buffer": 0, "byteOffset": 72, "byteLength": 9 },
        ],
        "materials": [
            { "name": "Mat0", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } },
            { "name": "Mat1", "pbrMetallicRoughness": { "baseColorFactor": [0.5, 0.5, 0.5, 1.0] } },
        ],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic quantized-normal glTF JSON");

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
        "manifold_synthetic_quantized_normal_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic quantized-normal glb to temp dir");
    path
}
/// Build a minimal, valid `.glb` with ONE triangle primitive that has NO
/// `material` key at all — glTF's implicit default material
/// (GLB_XFAIL_BURNDOWN_DESIGN.md D4, BUG-171). No `materials` array in
/// the document at all, matching a real asset like `BoxVertexColors.glb`
/// that carries geometry but declares zero materials. Same hand-rolled
/// binary-container shape as `write_synthetic_multimaterial_glb`.
pub(super) fn write_synthetic_default_material_glb() -> std::path::PathBuf {
    let tri: [[f32; 3]; 3] = [[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let mut bin = Vec::with_capacity(36);
    for v in &tri {
        for c in v {
            bin.extend_from_slice(&c.to_le_bytes());
        }
    }

    let doc = serde_json::json!({
        "asset": { "version": "2.0" },
        "scene": 0,
        "scenes": [{ "nodes": [0] }],
        "nodes": [{ "mesh": 0 }],
        // No "material" key on the primitive — glTF's implicit default
        // material. No "materials" array at all, matching a real asset
        // that declares zero materials.
        "meshes": [{ "primitives": [{ "attributes": { "POSITION": 0 } }] }],
        "accessors": [{
            "bufferView": 0,
            "componentType": 5126,
            "count": 3,
            "type": "VEC3",
            "min": [0.0, 0.0, 0.0],
            "max": [1.0, 1.0, 0.0],
        }],
        "bufferViews": [{ "buffer": 0, "byteOffset": 0, "byteLength": 36 }],
        "buffers": [{ "byteLength": bin.len() }],
    });
    let json_bytes = serde_json::to_vec(&doc).expect("serialize synthetic glTF JSON");

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
        "manifold_synthetic_defaultmat_{}_{}.glb",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0)
    ));
    std::fs::write(&path, &glb).expect("write synthetic glb to temp dir");
    path
}
