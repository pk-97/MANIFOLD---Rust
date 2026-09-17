//! GPU proofs for native Metal render-pipeline cache identity.
//!
//! These tests deliberately use one device per construction order. That keeps
//! each order independent while still proving that a second descriptor cannot
//! reuse the first descriptor's cached PSO.

use super::*;
use crate::{
    GpuBinding, GpuBlendFactor, GpuBlendOp, GpuBlendState, GpuTexture, GpuTextureDesc,
    GpuTextureDimension, GpuTextureFormat, GpuTextureUsage, GpuVertexAttribute, GpuVertexFormat,
    GpuVertexLayout,
};

const BLEND_WGSL: &str = r#"
    struct Uniforms { color: vec4<f32>, };
    @group(0) @binding(0) var<uniform> uniforms: Uniforms;

    struct VsOut { @builtin(position) position: vec4<f32>, };

    @vertex
    fn vs_main(@builtin(vertex_index) index: u32) -> VsOut {
        var positions = array<vec2<f32>, 3>(
            vec2<f32>(-1.0, -1.0),
            vec2<f32>( 3.0, -1.0),
            vec2<f32>(-1.0,  3.0),
        );
        var out: VsOut;
        out.position = vec4<f32>(positions[index], 0.0, 1.0);
        return out;
    }

    @fragment
    fn fs_main() -> @location(0) vec4<f32> {
        return uniforms.color;
    }
"#;

const VERTEX_LAYOUT_WGSL: &str = r#"
    struct VertexIn {
        @location(0) position: vec2<f32>,
        @location(1) color: vec4<f32>,
    };
    struct VertexOut {
        @builtin(position) position: vec4<f32>,
        @location(0) color: vec4<f32>,
    };

    @vertex
    fn vs_main(input: VertexIn) -> VertexOut {
        var out: VertexOut;
        out.position = vec4<f32>(input.position, 0.0, 1.0);
        out.color = input.color;
        return out;
    }

    @fragment
    fn fs_main(input: VertexOut) -> @location(0) vec4<f32> {
        return input.color;
    }
"#;

const BACKGROUND: [u8; 16] = [
    0xcd, 0xcc, 0x4c, 0x3e, // 0.2
    0x9a, 0x99, 0x99, 0x3e, // 0.3
    0xcd, 0xcc, 0xcc, 0x3e, // 0.4
    0x00, 0x00, 0x80, 0x3f, // 1.0
];
const FOREGROUND: [u8; 16] = [
    0xcd, 0xcc, 0x4c, 0x3f, // 0.8
    0xcd, 0xcc, 0x4c, 0x3e, // 0.2
    0x9a, 0x99, 0x19, 0x3f, // 0.6
    0x00, 0x00, 0x00, 0x3f, // 0.5
];

fn rgba8_target(device: &GpuDevice, format: GpuTextureFormat, label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: 4,
        height: 4,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET_FULL,
        label,
        mip_levels: 1,
    })
}

fn render_fullscreen(
    device: &GpuDevice,
    pipeline: &GpuRenderPipeline,
    target: &GpuTexture,
    color: &[u8; 16],
    clear: bool,
    label: &str,
) {
    let mut encoder = device.create_encoder(label);
    encoder.draw_fullscreen(
        pipeline,
        target,
        &[GpuBinding::Bytes {
            binding: 0,
            data: color,
        }],
        clear,
        true,
        label,
    );
    encoder.commit_and_wait_completed();
}

fn read_first_pixel(device: &GpuDevice, target: &GpuTexture) -> [u8; 4] {
    let bytes_per_row = target.width * target.format.bytes_per_pixel();
    let buffer = device.create_buffer_shared(bytes_per_row as u64 * target.height as u64);
    let mut encoder = device.create_encoder("pipeline-cache-readback");
    encoder.copy_texture_to_buffer(target, &buffer, target.width, target.height, bytes_per_row);
    encoder.commit_and_wait_completed();

    let ptr = buffer
        .mapped_ptr()
        .expect("shared readback buffer must be CPU mapped");
    let pixel = unsafe { std::slice::from_raw_parts(ptr, 4) };
    [pixel[0], pixel[1], pixel[2], pixel[3]]
}

fn blend_state(operation: GpuBlendOp) -> GpuBlendState {
    GpuBlendState {
        src_factor: GpuBlendFactor::One,
        dst_factor: GpuBlendFactor::One,
        operation,
        src_alpha_factor: GpuBlendFactor::One,
        dst_alpha_factor: GpuBlendFactor::One,
        alpha_operation: operation,
    }
}

fn assert_near(actual: [u8; 4], expected: [u8; 4], label: &str) {
    for (index, (&actual, &expected)) in actual.iter().zip(expected.iter()).enumerate() {
        let delta = actual.abs_diff(expected);
        assert!(
            delta <= 2,
            "{label}: channel {index} was {actual}, expected {expected} (pixel={actual:?})"
        );
    }
}

fn run_blend_order(min_first: bool) {
    let device = GpuDevice::new();
    let none = device.create_render_pipeline(
        BLEND_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba8Unorm,
        None,
        "blend-none-first",
    );
    let none_again = device.create_render_pipeline(
        BLEND_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba8Unorm,
        None,
        "blend-none-second-label-is-ignored",
    );
    assert!(
        std::ptr::eq(none.raw_state(), none_again.raw_state()),
        "identical descriptors must reuse the cached pipeline despite labels"
    );

    let min = blend_state(GpuBlendOp::Min);
    let max = blend_state(GpuBlendOp::Max);
    let (min_pipeline, max_pipeline) = if min_first {
        let min_pipeline = device.create_render_pipeline(
            BLEND_WGSL,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba8Unorm,
            Some(min),
            "blend-min-first",
        );
        let max_pipeline = device.create_render_pipeline(
            BLEND_WGSL,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba8Unorm,
            Some(max),
            "blend-max-second",
        );
        (min_pipeline, max_pipeline)
    } else {
        let max_pipeline = device.create_render_pipeline(
            BLEND_WGSL,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba8Unorm,
            Some(max),
            "blend-max-first",
        );
        let min_pipeline = device.create_render_pipeline(
            BLEND_WGSL,
            "vs_main",
            "fs_main",
            GpuTextureFormat::Rgba8Unorm,
            Some(min),
            "blend-min-second",
        );
        (min_pipeline, max_pipeline)
    };

    assert!(
        !std::ptr::eq(min_pipeline.raw_state(), max_pipeline.raw_state()),
        "Min and Max blend descriptors must not alias regardless of request order"
    );
    assert!(
        !std::ptr::eq(none.raw_state(), min_pipeline.raw_state())
            && !std::ptr::eq(none.raw_state(), max_pipeline.raw_state()),
        "None, Min, and Max blend descriptors must each have a distinct pipeline"
    );

    let target = rgba8_target(
        &device,
        GpuTextureFormat::Rgba8Unorm,
        "blend-readback-target",
    );
    render_fullscreen(
        &device,
        &none,
        &target,
        &BACKGROUND,
        true,
        "blend-background",
    );
    render_fullscreen(
        &device,
        &min_pipeline,
        &target,
        &FOREGROUND,
        false,
        "blend-min-readback",
    );
    assert_near(
        read_first_pixel(&device, &target),
        [51, 51, 102, 128],
        "Min blend",
    );

    render_fullscreen(
        &device,
        &none,
        &target,
        &BACKGROUND,
        true,
        "blend-background-again",
    );
    render_fullscreen(
        &device,
        &max_pipeline,
        &target,
        &FOREGROUND,
        false,
        "blend-max-readback",
    );
    assert_near(
        read_first_pixel(&device, &target),
        [204, 77, 153, 255],
        "Max blend",
    );
}

#[test]
fn render_pipeline_cache_identity_and_readback() {
    run_blend_order(true);
    run_blend_order(false);

    let device = GpuDevice::new();
    let rgba = device.create_render_pipeline(
        BLEND_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba8Unorm,
        None,
        "rgba-format",
    );
    let bgra = device.create_render_pipeline(
        BLEND_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Bgra8Unorm,
        None,
        "bgra-format",
    );
    assert!(
        !std::ptr::eq(rgba.raw_state(), bgra.raw_state()),
        "different color attachment formats must not alias"
    );

    let first_layout = GpuVertexLayout {
        stride: 32,
        attributes: vec![
            GpuVertexAttribute {
                format: GpuVertexFormat::Float32x2,
                offset: 0,
                shader_location: 0,
            },
            GpuVertexAttribute {
                format: GpuVertexFormat::Float32x4,
                offset: 8,
                shader_location: 1,
            },
        ],
    };
    let second_layout = GpuVertexLayout {
        stride: 32,
        attributes: vec![
            GpuVertexAttribute {
                format: GpuVertexFormat::Float32x2,
                offset: 4,
                shader_location: 0,
            },
            GpuVertexAttribute {
                format: GpuVertexFormat::Float32x4,
                offset: 12,
                shader_location: 1,
            },
        ],
    };
    let first_layout_pipeline = device.create_render_pipeline_with_vertex_layout(
        VERTEX_LAYOUT_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba8Unorm,
        None,
        &first_layout,
        "vertex-layout-first",
    );
    let second_layout_pipeline = device.create_render_pipeline_with_vertex_layout(
        VERTEX_LAYOUT_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba8Unorm,
        None,
        &second_layout,
        "vertex-layout-second",
    );
    assert!(
        !std::ptr::eq(
            first_layout_pipeline.raw_state(),
            second_layout_pipeline.raw_state()
        ),
        "same-stride vertex layouts with different offsets must not alias"
    );

    let depth_min = device.create_render_pipeline_depth(
        BLEND_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba8Unorm,
        GpuTextureFormat::Depth32Float,
        Some(blend_state(GpuBlendOp::Min)),
        1,
        "depth-min",
    );
    let depth_max = device.create_render_pipeline_depth(
        BLEND_WGSL,
        "vs_main",
        "fs_main",
        GpuTextureFormat::Rgba8Unorm,
        GpuTextureFormat::Depth32Float,
        Some(blend_state(GpuBlendOp::Max)),
        1,
        "depth-max",
    );
    assert!(
        !std::ptr::eq(depth_min.raw_state(), depth_max.raw_state()),
        "depth pipeline blend variants must not alias"
    );

    let rgba_target = rgba8_target(&device, GpuTextureFormat::Rgba8Unorm, "rgba-format-target");
    render_fullscreen(
        &device,
        &rgba,
        &rgba_target,
        &FOREGROUND,
        true,
        "rgba-format-draw",
    );
    assert_near(
        read_first_pixel(&device, &rgba_target),
        [204, 51, 153, 128],
        "RGBA8 readback",
    );

    let bgra_target = rgba8_target(&device, GpuTextureFormat::Bgra8Unorm, "bgra-format-target");
    render_fullscreen(
        &device,
        &bgra,
        &bgra_target,
        &FOREGROUND,
        true,
        "bgra-format-draw",
    );
    assert_near(
        read_first_pixel(&device, &bgra_target),
        [153, 51, 204, 128],
        "BGRA8 readback",
    );
}
