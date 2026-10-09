use super::*;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

pub(crate) struct TextureFixture {
    pub path: PathBuf,
    root: PathBuf,
}

impl TextureFixture {
    pub fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "manifold-texture-test-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self {
            path: root.join("scene.gltf"),
            root,
        }
    }

    pub fn image_path(&self) -> PathBuf {
        self.root.join("map.png")
    }

    pub fn write_png(&self, value: u8) {
        image::RgbaImage::from_pixel(2, 1, image::Rgba([value, 23, 47, 129]))
            .save(self.image_path())
            .unwrap();
    }

    pub fn write_document(
        &self,
        textures: serde_json::Value,
        images: serde_json::Value,
        extra_bytes: &[u8],
    ) {
        let positions = [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
        let mut bytes = bytemuck::cast_slice(&positions).to_vec();
        bytes.extend_from_slice(extra_bytes);
        std::fs::write(self.root.join("mesh.bin"), &bytes).unwrap();
        let mut views = vec![serde_json::json!({"buffer":0,"byteOffset":0,"byteLength":36})];
        if !extra_bytes.is_empty() {
            views.push(
                serde_json::json!({"buffer":0,"byteOffset":36,"byteLength":extra_bytes.len()}),
            );
        }
        let document = serde_json::json!({
            "asset":{"version":"2.0"}, "scene":0, "scenes":[{"nodes":[0]}],
            "nodes":[{"mesh":0}], "meshes":[{"primitives":[{"attributes":{"POSITION":0}}]}],
            "accessors":[{"bufferView":0,"componentType":5126,"count":3,"type":"VEC3","min":[0,0,0],"max":[1,1,0]}],
            "buffers":[{"uri":"mesh.bin","byteLength":bytes.len()}], "bufferViews":views,
            "textures":textures, "images":images, "extensionsUsed":["EXT_texture_webp"]
        });
        std::fs::write(&self.path, serde_json::to_vec(&document).unwrap()).unwrap();
    }

    pub fn external_png() -> Self {
        let fixture = Self::new();
        fixture.write_png(17);
        fixture.write_document(
            serde_json::json!([{"source":0}]),
            serde_json::json!([{"uri":"map.png"}]),
            &[],
        );
        fixture
    }
}

impl Drop for TextureFixture {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.root).unwrap();
    }
}

#[test]
fn luma_alpha_png_expands_luminance_and_preserves_alpha() {
    let fixture = TextureFixture::external_png();
    image::GrayAlphaImage::from_raw(2, 1, vec![51, 19, 203, 231])
        .unwrap()
        .save(fixture.image_path())
        .unwrap();
    let (w, h, rgba) = load_gltf_texture(&fixture.path, 0).unwrap();
    assert_eq!((w, h), (2, 1));
    assert_eq!(rgba, [51, 51, 51, 19, 203, 203, 203, 231]);
}

#[test]
fn selected_texture_decodes_one_image_and_ignores_unrelated_missing_images() {
    let fixture = TextureFixture::external_png();
    fixture.write_document(
        serde_json::json!([{"source":0}]),
        serde_json::json!([
            {"uri":"map.png"}, {"uri":"does-not-exist.png"}, {"uri":"also-missing.png"}
        ]),
        &[],
    );
    TEXTURE_DECODE_COUNT.with(|count| count.set(0));
    let (_, _, rgba) = load_gltf_texture(&fixture.path, 0).unwrap();
    assert_eq!(rgba[0], 17);
    assert_eq!(TEXTURE_DECODE_COUNT.with(|count| count.get()), 1);
}

#[test]
fn webp_dimensions_match_runtime_with_absent_or_different_size_fallback() {
    let fixture = TextureFixture::external_png();
    let mut webp = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(3, 2, image::Rgba([7, 33, 99, 255]))
        .write_to(&mut webp, image::ImageFormat::WebP)
        .unwrap();
    let images = serde_json::json!([{"uri":"map.png"}, {"bufferView":1,"mimeType":"image/webp"}]);
    for texture in [
        serde_json::json!({"extensions":{"EXT_texture_webp":{"source":1}}}),
        serde_json::json!({"source":0,"extensions":{"EXT_texture_webp":{"source":1}}}),
    ] {
        fixture.write_document(serde_json::json!([texture]), images.clone(), webp.get_ref());
        let summary = gltf_import_summary(&fixture.path).unwrap();
        let (w, h, _) = load_gltf_texture(&fixture.path, 0).unwrap();
        assert_eq!((w, h), (3, 2));
        assert_eq!(summary.texture_dims, [(w, h)]);
    }
}

#[test]
fn missing_texture_source_is_reported_by_summary_and_runtime() {
    let fixture = TextureFixture::external_png();
    fixture.write_document(
        serde_json::json!([{}]),
        serde_json::json!([{"uri":"map.png"}]),
        &[],
    );
    let summary = gltf_import_summary(&fixture.path).unwrap();
    let error = load_gltf_texture(&fixture.path, 0).unwrap_err();
    assert_eq!(error, "texture 0 has no image source");
    assert!(summary.extension_report_lines.contains(&error));
    assert_eq!(summary.texture_dims, [(2, 2)]);
}

#[test]
fn texture_snapshot_owns_selected_dependency_bytes() {
    let fixture = TextureFixture::external_png();
    let original = parse_texture_snapshot(&fixture.path, 0).unwrap();
    fixture.write_png(201);
    let edited = parse_texture_snapshot(&fixture.path, 0).unwrap();
    assert_ne!(original.identity, edited.identity);
    assert_eq!(decode_texture_snapshot(original).unwrap().2[0], 17);
    assert_eq!(decode_texture_snapshot(edited).unwrap().2[0], 201);
    std::fs::remove_file(fixture.image_path()).unwrap();
    assert!(parse_texture_snapshot(&fixture.path, 0).is_err());
    // Restore a valid source and prove failures are not sticky.
    fixture.write_png(92);
    assert_eq!(
        load_gltf_texture(Path::new(&fixture.path), 0).unwrap().2[0],
        92
    );
}

#[test]
fn relative_uri_validation_matches_pinned_importer_rules() {
    for uri in [
        "map.png",
        "a+b.png",
        "a%20b.png",
        "%C3%A9.png",
        "%",
        "%F",
        "%FZ",
        "%GG",
        "file:%FF.png",
        "data:image/png;base64,AAAA",
    ] {
        assert!(validate_import_uri(uri).is_ok(), "{uri}");
    }
    for uri in ["%FF.png", "%C3.png", "%80.png", "é%FF.png"] {
        assert!(validate_import_uri(uri).is_err(), "{uri}");
    }
}

#[test]
fn malformed_relative_image_uri_reports_error_without_unwinding() {
    let fixture = TextureFixture::external_png();
    fixture.write_document(
        serde_json::json!([{ "source": 0 }]),
        serde_json::json!([{ "uri": "%FF.png" }]),
        &[],
    );
    let result = std::panic::catch_unwind(|| parse_texture_snapshot(&fixture.path, 0))
        .expect("malformed relative image URI must not panic");
    assert!(result.err().unwrap().contains("invalid resource URI"));
    let summary = gltf_import_summary(&fixture.path).unwrap();
    assert!(
        summary
            .extension_report_lines
            .iter()
            .any(|line| line.contains("invalid resource URI"))
    );
}
