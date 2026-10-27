//! The cover draws for the pick region and the selection mask: one
//! full-screen triangle per field, marching the same field as Pass B.
use std::sync::LazyLock;

use super::water_march::{FieldGeometry, FieldView, MarchUniforms, march_uniforms};
use super::*;
use manifold_node_engine::scene::world_tiles::READ_SIDE;

pub(super) static COVER_WGSL: LazyLock<String> = LazyLock::new(|| {
    [
        READ_SIDE,
        FIELD_TILES_WGSL,
        include_str!("../shaders/water_march_core.wgsl"),
        include_str!("../shaders/water_cover.wgsl"),
    ]
    .concat()
});

/// The march uniforms for a cover draw of `field` through `view_proj`, a
/// target of `width` x `height` pixels. Flags stay zero: a cover writes no
/// depth or velocity outputs.
pub(super) fn cover_uniforms(
    view_proj: [[f32; 4]; 4],
    width: u32,
    height: u32,
    field: &FieldObject,
) -> Result<MarchUniforms, &'static str> {
    let view = FieldView {
        view_proj,
        prev_view_proj: view_proj,
        jitter_ndc: [0.0; 2],
        prev_jitter_ndc: [0.0; 2],
        frame_seconds: 0.0,
        width,
        height,
    };
    march_uniforms(
        &view,
        &FieldGeometry {
            pool_tiles: field.pool_tiles,
            cell_size: field.cell_size,
            model: field.model,
            prev_model: field.prev_model,
            slot: field.slot,
            has_velocity: false,
        },
    )
}

/// The cover's bindings: the uniforms, the field's level set, its world-tile
/// table and its extent.
pub(super) fn cover_bindings<'a>(
    uniforms: &'a MarchUniforms,
    field: &FieldObject<'a>,
) -> [GpuBinding<'a>; 4] {
    [
        GpuBinding::Bytes {
            binding: 0,
            data: bytemuck::bytes_of(uniforms),
        },
        GpuBinding::Buffer {
            binding: 1,
            buffer: field.levelset,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 70,
            buffer: field.tiles,
            offset: 0,
        },
        GpuBinding::Buffer {
            binding: 71,
            buffer: field.extent,
            offset: 0,
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cover_wgsl_parses_and_validates() {
        let src = COVER_WGSL.as_str();
        let module = naga::front::wgsl::parse_str(src).expect("water cover must naga-parse");
        let mut validator = naga::valid::Validator::new(
            naga::valid::ValidationFlags::all(),
            naga::valid::Capabilities::all(),
        );
        if let Err(e) = validator.validate(&module) {
            panic!("water cover validation failed:\n{}", e.emit_to_string(src));
        }
        for entry in ["vs_cover", "fs_cover_id", "fs_cover_mask"] {
            assert!(
                module.entry_points.iter().any(|ep| ep.name == entry),
                "missing entry point {entry}"
            );
        }
    }
}
