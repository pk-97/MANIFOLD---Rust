//! Render parity (design section 10): each test draws a tiny target whose
//! pixels are known, on whichever backend this build has, so Metal shows the
//! expected values and Vulkan must match them. On Vulkan each also ends with
//! no validation errors.

use crate::*;

fn assert_clean() {
    #[cfg(vulkan_backend)]
    assert_eq!(
        crate::vulkan::validation_error_count(),
        0,
        "validation errors, see the log"
    );
}

const SIZE: u32 = 8;

fn target(device: &GpuDevice, format: GpuTextureFormat, label: &str) -> GpuTexture {
    device.create_texture(&GpuTextureDesc {
        width: SIZE,
        height: SIZE,
        depth: 1,
        format,
        dimension: GpuTextureDimension::D2,
        usage: if format == GpuTextureFormat::Depth32Float {
            GpuTextureUsage::RENDER_TARGET | GpuTextureUsage::SHADER_READ
        } else {
            GpuTextureUsage::RENDER_TARGET_FULL
        },
        label,
        mip_levels: 1,
    })
}

/// Rgba8 pixels of an 8×8 target, row 0 first.
fn pixels(device: &GpuDevice, texture: &GpuTexture) -> Vec<[u8; 4]> {
    let buffer = device.create_buffer(u64::from(SIZE * SIZE * 4), BufferUse::Readback);
    let mut enc = device.create_encoder("render parity readback");
    enc.copy_texture_to_buffer(texture, &buffer, SIZE, SIZE, SIZE * 4);
    enc.commit_and_wait().expect("GPU work completes");
    let ptr = buffer.mapped_ptr().expect("readback buffers map");
    unsafe { std::slice::from_raw_parts(ptr.cast::<[u8; 4]>(), (SIZE * SIZE) as usize) }.to_vec()
}

/// Depth values of an 8×8 depth target, through an R32Float copy.
fn depths(device: &GpuDevice, depth: &GpuTexture) -> Vec<f32> {
    let float = device.create_texture(&GpuTextureDesc {
        width: SIZE,
        height: SIZE,
        depth: 1,
        format: GpuTextureFormat::R32Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_READ | GpuTextureUsage::SHADER_WRITE,
        label: "render parity depth copy",
        mip_levels: 1,
    });
    let buffer = device.create_buffer(u64::from(SIZE * SIZE * 4), BufferUse::Readback);
    let mut enc = device.create_encoder("render parity depth readback");
    enc.copy_depth_to_float(depth, &float);
    enc.copy_texture_to_buffer(&float, &buffer, SIZE, SIZE, SIZE * 4);
    enc.commit_and_wait().expect("GPU work completes");
    let ptr = buffer.mapped_ptr().expect("readback buffers map");
    unsafe { std::slice::from_raw_parts(ptr.cast::<f32>(), (SIZE * SIZE) as usize) }.to_vec()
}

/// Triangles, lines or points from a uniform list of clip-space corners
/// (x, y, z, pad), coloured by the uniform; points are 4 pixels wide.
const SHAPES: &str = r#"
struct Shape { color: vec4<f32>, corners: array<vec4<f32>, 6> }
@group(0) @binding(0) var<uniform> shape: Shape;
struct Out { @builtin(position) position: vec4<f32>, @location(0) size: f32 }
@vertex fn vs_main(@builtin(vertex_index) i: u32) -> Out {
    let c = shape.corners[i];
    return Out(vec4<f32>(c.xyz, 1.0), 4.0);
}
@fragment fn fs_main() -> @location(0) vec4<f32> { return shape.color; }
@fragment fn fs_depth() {}
"#;

fn shape(color: [f32; 4], corners: &[[f32; 3]]) -> Vec<u8> {
    let mut words = color.to_vec();
    for c in corners {
        words.extend_from_slice(&[c[0], c[1], c[2], 0.0]);
    }
    words.resize(4 + 6 * 4, 0.0);
    words.iter().flat_map(|w| w.to_ne_bytes()).collect()
}

fn uniform(bytes: &[u8]) -> [GpuBinding<'_>; 1] {
    [GpuBinding::Bytes {
        binding: 0,
        data: bytes,
    }]
}

/// Clip-space rectangle as two counter-clockwise triangles at depth `z`.
fn rect(x0: f32, y0: f32, x1: f32, y1: f32, z: f32) -> [[f32; 3]; 6] {
    [
        [x0, y0, z],
        [x1, y0, z],
        [x0, y1, z],
        [x1, y0, z],
        [x1, y1, z],
        [x0, y1, z],
    ]
}

fn colour_pipeline(device: &GpuDevice, format: GpuTextureFormat) -> GpuRenderPipeline {
    device.create_render_pipeline(
        &RenderPipelineDesc::new(SHAPES, "vs_main", "fs_main", "render parity shapes")
            .color(format, None),
    )
}

const RED: [u8; 4] = [255, 0, 0, 255];
const GREEN: [u8; 4] = [0, 255, 0, 255];
const BLUE: [u8; 4] = [0, 0, 255, 255];
const CLEAR: [u8; 4] = [0, 0, 0, 0];

#[test]
fn parity_render_clip_space_is_y_up_and_back_faces_cull() {
    let device = GpuDevice::new();
    let pipeline = colour_pipeline(&device, GpuTextureFormat::Rgba8Unorm);
    let out = target(&device, GpuTextureFormat::Rgba8Unorm, "render parity y");
    // The top half (clip y > 0) in red; then, culling back faces, a
    // counter-clockwise bottom-left quarter in green and the same quarter
    // wound clockwise on the bottom right, which must not draw.
    let top = shape([1.0, 0.0, 0.0, 1.0], &rect(-1.0, 0.0, 1.0, 1.0, 0.5));
    let left = shape([0.0, 1.0, 0.0, 1.0], &rect(-1.0, -1.0, 0.0, 0.0, 0.5));
    let mut cw = rect(0.0, -1.0, 1.0, 0.0, 0.5);
    cw.swap(1, 2);
    cw.swap(4, 5);
    let right = shape([0.0, 1.0, 0.0, 1.0], &cw);
    let (top_b, left_b, right_b) = (uniform(&top), uniform(&left), uniform(&right));
    let mut enc = device.create_encoder("render parity y");
    enc.draw(
        &DrawPass::color(&out, GpuLoadAction::Clear),
        &[
            Draw::new(&pipeline, &top_b, 6, 1),
            Draw::new(&pipeline, &left_b, 6, 1).cull_back_faces(true),
            Draw::new(&pipeline, &right_b, 6, 1).cull_back_faces(true),
        ],
        "render parity y",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let px = pixels(&device, &out);
    let at = |x: u32, y: u32| px[(y * SIZE + x) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            let want = match (y < SIZE / 2, x < SIZE / 2) {
                (true, _) => RED,
                (false, true) => GREEN,
                (false, false) => CLEAR,
            };
            assert_eq!(at(x, y), want, "pixel ({x}, {y})");
        }
    }
    assert_clean();
}

#[test]
fn parity_render_viewport_blend_and_load() {
    let device = GpuDevice::new();
    let add = GpuBlendState {
        src_factor: GpuBlendFactor::One,
        dst_factor: GpuBlendFactor::One,
        operation: GpuBlendOp::Add,
        src_alpha_factor: GpuBlendFactor::One,
        dst_alpha_factor: GpuBlendFactor::One,
        alpha_operation: GpuBlendOp::Max,
    };
    let plain = colour_pipeline(&device, GpuTextureFormat::Rgba8Unorm);
    let additive = device.create_render_pipeline(
        &RenderPipelineDesc::new(SHAPES, "vs_main", "fs_main", "render parity add")
            .color(GpuTextureFormat::Rgba8Unorm, Some(add)),
    );
    let out = target(&device, GpuTextureFormat::Rgba8Unorm, "render parity blend");
    let full = rect(-1.0, -1.0, 1.0, 1.0, 0.5);
    let red = shape([1.0, 0.0, 0.0, 1.0], &full);
    let blue = shape([0.0, 0.0, 1.0, 1.0], &full);
    let (red_b, blue_b) = (uniform(&red), uniform(&blue));
    let mut enc = device.create_encoder("render parity blend");
    enc.draw(
        &DrawPass::color(&out, GpuLoadAction::Clear),
        &[Draw::new(&plain, &red_b, 6, 1)],
        "render parity red",
    );
    // A second pass loads the red and adds blue in the top-left quarter.
    enc.draw(
        &DrawPass::color(&out, GpuLoadAction::Load).viewport((0.0, 0.0, 4.0, 4.0)),
        &[Draw::new(&additive, &blue_b, 6, 1)],
        "render parity add",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let px = pixels(&device, &out);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let want = if x < 4 && y < 4 {
                [255, 0, 255, 255]
            } else {
                RED
            };
            assert_eq!(px[(y * SIZE + x) as usize], want, "pixel ({x}, {y})");
        }
    }
    assert_clean();
}

#[test]
fn parity_render_depth_msaa_resolves_and_second_group() {
    let device = GpuDevice::new();
    let pipeline = device.create_render_pipeline(
        &RenderPipelineDesc::new(SHAPES, "vs_main", "fs_main", "render parity depth")
            .color(GpuTextureFormat::Rgba8Unorm, None)
            .depth(GpuTextureFormat::Depth32Float)
            .samples(4),
    );
    let less = device.create_depth_stencil_state(&GpuDepthStencilDesc {
        compare: GpuCompareFunction::Less,
        write_enabled: true,
    });
    let read_only = device.create_depth_stencil_state(&GpuDepthStencilDesc {
        compare: GpuCompareFunction::LessEqual,
        write_enabled: false,
    });
    let msaa = device.create_texture_msaa(
        SIZE,
        SIZE,
        GpuTextureFormat::Rgba8Unorm,
        4,
        "render parity msaa",
    );
    let msaa_depth = device.create_texture_msaa_memoryless(
        SIZE,
        SIZE,
        GpuTextureFormat::Depth32Float,
        4,
        "render parity msaa depth",
    );
    let resolved = target(
        &device,
        GpuTextureFormat::Rgba8Unorm,
        "render parity resolved",
    );
    let resolved_depth = target(
        &device,
        GpuTextureFormat::Depth32Float,
        "render parity resolved depth",
    );
    // Red at 0.5 over the left half, then green behind it at 0.75 over the
    // whole target; the second group (no depth write) draws blue at 0.25
    // over the top half.
    let red = shape([1.0, 0.0, 0.0, 1.0], &rect(-1.0, -1.0, 0.0, 1.0, 0.5));
    let green = shape([0.0, 1.0, 0.0, 1.0], &rect(-1.0, -1.0, 1.0, 1.0, 0.75));
    let blue = shape([0.0, 0.0, 1.0, 1.0], &rect(-1.0, 0.0, 1.0, 1.0, 0.25));
    let (red_b, green_b, blue_b) = (uniform(&red), uniform(&green), uniform(&blue));
    let second = [Draw::new(&pipeline, &blue_b, 6, 1)];
    let mut enc = device.create_encoder("render parity depth");
    enc.draw(
        &DrawPass {
            second_group: Some((&read_only, &second)),
            ..DrawPass::msaa(&msaa, &resolved, GpuLoadAction::Clear).depth(
                &msaa_depth,
                &less,
                GpuLoadAction::Clear,
                DepthStore::Resolve(&resolved_depth),
            )
        },
        &[
            Draw::new(&pipeline, &red_b, 6, 1),
            Draw::new(&pipeline, &green_b, 6, 1),
        ],
        "render parity depth",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let px = pixels(&device, &resolved);
    let depth = depths(&device, &resolved_depth);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let i = (y * SIZE + x) as usize;
            let (want, want_depth) = match (y < SIZE / 2, x < SIZE / 2) {
                (true, true) => (BLUE, 0.5),
                (true, false) => (BLUE, 0.75),
                (false, true) => (RED, 0.5),
                (false, false) => (GREEN, 0.75),
            };
            assert_eq!(px[i], want, "pixel ({x}, {y})");
            assert_eq!(depth[i], want_depth, "depth ({x}, {y})");
        }
    }
    assert_clean();
}

#[test]
fn parity_render_depth_only_viewport_and_bias() {
    let device = GpuDevice::new();
    let pipeline = device.create_render_pipeline(
        &RenderPipelineDesc::new(SHAPES, "vs_main", "fs_depth", "render parity depth only")
            .depth(GpuTextureFormat::Depth32Float),
    );
    let less = device.create_depth_stencil_state(&GpuDepthStencilDesc {
        compare: GpuCompareFunction::Less,
        write_enabled: true,
    });
    let depth = target(
        &device,
        GpuTextureFormat::Depth32Float,
        "render parity shadow",
    );
    let near = shape([0.0; 4], &rect(-1.0, -1.0, 1.0, 1.0, 0.25));
    let biased = shape([0.0; 4], &rect(-1.0, -1.0, 1.0, 1.0, 0.5));
    let (near_b, biased_b) = (uniform(&near), uniform(&biased));
    let mut enc = device.create_encoder("render parity depth only");
    // The top-left quarter at 0.25, wireframe and points ignored; then the
    // rest of the target, biased away from 0.5 in a second pass.
    enc.draw(
        &DrawPass::depth_only(&depth, &less, GpuLoadAction::Clear).viewport((0.0, 0.0, 4.0, 4.0)),
        &[Draw::new(&pipeline, &near_b, 6, 1).fill_mode(GpuTriangleFillMode::Lines)],
        "render parity near",
    );
    enc.draw(
        &DrawPass::depth_only(&depth, &less, GpuLoadAction::Load),
        &[Draw::new(&pipeline, &biased_b, 6, 1).depth_bias(1000.0, 0.0, 0.0)],
        "render parity biased",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let values = depths(&device, &depth);
    // One unit of constant bias on a float depth in [0.5, 1) is 2^-24.
    let bias = 1000.0 * 2f32.powi(-24);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let got = values[(y * SIZE + x) as usize];
            if x < 4 && y < 4 {
                assert_eq!(got, 0.25, "depth ({x}, {y})");
            } else {
                assert!(
                    (got - 0.5 - bias).abs() < 0.1 * bias,
                    "depth ({x}, {y}): {got}, want 0.5 + {bias}"
                );
            }
        }
    }
    assert_clean();
}

/// Vertices from a buffer through a vertex layout, drawn indexed.
const LAID_OUT: &str = r#"
struct In { @location(0) position: vec2<f32>, @location(1) color: vec4<f32> }
struct Out { @builtin(position) position: vec4<f32>, @location(0) color: vec4<f32> }
@group(0) @binding(0) var tex: texture_2d<f32>;
@vertex fn vs_main(v: In) -> Out { return Out(vec4<f32>(v.position, 0.5, 1.0), v.color); }
@fragment fn fs_main(v: Out) -> @location(0) vec4<f32> {
    return v.color * textureLoad(tex, vec2<i32>(0, 0), 0);
}
"#;

fn quad_vertices(
    device: &GpuDevice,
    x0: f32,
    y0: f32,
    x1: f32,
    y1: f32,
    color: [f32; 4],
) -> GpuBuffer {
    let corners = [[x0, y0], [x1, y0], [x1, y1], [x0, y1]];
    let floats: Vec<f32> = corners
        .iter()
        .flat_map(|c| [c[0], c[1], color[0], color[1], color[2], color[3]])
        .collect();
    let buffer = device.create_buffer(floats.len() as u64 * 4, BufferUse::Upload);
    unsafe { buffer.write(0, bytemuck::cast_slice(&floats)) };
    buffer
}

#[test]
fn parity_render_long_pass_indexed_scissor_and_mid_pass_barrier() {
    let device = GpuDevice::new();
    let layout = GpuVertexLayout {
        stride: 24,
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
    let pipeline = device.create_render_pipeline(
        &RenderPipelineDesc::new(LAID_OUT, "vs_main", "fs_main", "render parity ui")
            .color(GpuTextureFormat::Rgba8Unorm, None)
            .vertex_layout(&layout),
    );
    let white = target(&device, GpuTextureFormat::Rgba8Unorm, "render parity white");
    let out = target(&device, GpuTextureFormat::Rgba8Unorm, "render parity ui");
    let full = quad_vertices(&device, -1.0, -1.0, 1.0, 1.0, [1.0, 0.0, 0.0, 1.0]);
    let left = quad_vertices(&device, -1.0, -1.0, 1.0, 1.0, [0.0, 1.0, 0.0, 1.0]);
    let indices = device.create_buffer(24, BufferUse::Upload);
    unsafe { indices.write(0, bytemuck::cast_slice(&[0u32, 1, 2, 0, 2, 3])) };
    let bindings = [GpuBinding::Texture {
        binding: 0,
        texture: &white,
    }];
    let mut enc = device.create_encoder("render parity ui");
    enc.clear_texture(&out, 0.0, 0.0, 1.0, 1.0);
    // Cleared before the pass and first sampled inside it: the draw needs a
    // barrier the pass began without.
    enc.clear_texture(&white, 1.0, 1.0, 1.0, 1.0);
    enc.begin_render_pass(&out, GpuLoadAction::Load, "render parity ui");
    enc.draw_in_render_pass(&pipeline, &bindings, &full, 0, &indices, 6, 0, None, "full");
    enc.set_scissor_rect(0, 0, 4, SIZE);
    enc.draw_in_render_pass(&pipeline, &bindings, &left, 0, &indices, 6, 0, None, "left");
    enc.end_render_pass();
    enc.commit_and_wait().expect("GPU work completes");
    let px = pixels(&device, &out);
    for y in 0..SIZE {
        for x in 0..SIZE {
            let want = if x < 4 { GREEN } else { RED };
            assert_eq!(px[(y * SIZE + x) as usize], want, "pixel ({x}, {y})");
        }
    }
    assert_clean();
}

#[test]
fn parity_render_points_lines_and_indirect_counts() {
    let device = GpuDevice::new();
    let points = device.create_render_pipeline(
        &RenderPipelineDesc::new(SHAPES, "vs_main", "fs_main", "render parity points")
            .color(GpuTextureFormat::Rgba8Unorm, None)
            .depth(GpuTextureFormat::Depth32Float)
            .point_size(0),
    );
    let always = device.create_depth_stencil_state(&GpuDepthStencilDesc {
        compare: GpuCompareFunction::Always,
        write_enabled: false,
    });
    let plain = colour_pipeline(&device, GpuTextureFormat::Rgba8Unorm);
    let out = target(
        &device,
        GpuTextureFormat::Rgba8Unorm,
        "render parity points",
    );
    let depth = target(
        &device,
        GpuTextureFormat::Depth32Float,
        "render parity points depth",
    );
    // A size-4 point on the centre covers the middle 4×4; a line along the
    // pixel row y = 0 (clip y = 7/8); the indirect draw's count comes from a
    // compute pass earlier in the same encoder.
    let dot = shape([0.0, 0.0, 1.0, 1.0], &[[0.0, 0.0, 0.5]]);
    let line = shape(
        [0.0, 1.0, 0.0, 1.0],
        &[[-1.0, 0.875, 0.5], [1.0, 0.875, 0.5]],
    );
    let corner = shape([1.0, 0.0, 0.0, 1.0], &rect(-1.0, -1.0, -0.5, -0.5, 0.5));
    const ARGS: &str = r#"
@group(0) @binding(0) var<storage, read_write> args: array<u32>;
@compute @workgroup_size(1) fn main() { args[0] = 6u; args[1] = 1u; args[2] = 0u; args[3] = 0u; }
"#;
    let args_pipeline = device.create_compute_pipeline(ARGS, "main", "render parity args");
    let args = device.create_buffer(16, BufferUse::Gpu);
    let (dot_b, line_b, corner_b) = (uniform(&dot), uniform(&line), uniform(&corner));
    let mut enc = device.create_encoder("render parity points");
    enc.dispatch_compute(
        &args_pipeline,
        &[GpuBinding::Buffer {
            binding: 0,
            buffer: &args,
            offset: 0,
        }],
        [1, 1, 1],
        "render parity args",
    );
    enc.draw(
        &DrawPass::color(&out, GpuLoadAction::Clear).depth(
            &depth,
            &always,
            GpuLoadAction::Clear,
            DepthStore::Discard,
        ),
        &[Draw::new(&points, &dot_b, 1, 1).primitive(GpuPrimitiveType::Point)],
        "render parity point",
    );
    enc.draw(
        &DrawPass::color(&out, GpuLoadAction::Load),
        &[
            Draw::new(&plain, &line_b, 2, 1).primitive(GpuPrimitiveType::Line),
            Draw::counted(
                &plain,
                &corner_b,
                DrawCount::Indirect {
                    args: &args,
                    offset: 0,
                },
            ),
        ],
        "render parity line",
    );
    enc.commit_and_wait().expect("GPU work completes");
    let px = pixels(&device, &out);
    let at = |x: u32, y: u32| px[(y * SIZE + x) as usize];
    for y in 0..SIZE {
        for x in 0..SIZE {
            if (2..6).contains(&x) && (2..6).contains(&y) {
                assert_eq!(at(x, y), BLUE, "point pixel ({x}, {y})");
            } else if x < 2 && y >= 6 {
                assert_eq!(at(x, y), RED, "indirect pixel ({x}, {y})");
            } else if y > 0 {
                assert_eq!(at(x, y), CLEAR, "pixel ({x}, {y})");
            }
        }
    }
    // The line's ends are the rasteriser's choice; its middle is not.
    for x in 1..SIZE - 1 {
        assert_eq!(at(x, 0), GREEN, "line pixel ({x}, 0)");
    }
    assert_clean();
}

#[test]
fn parity_generate_mipmaps_averages_each_level() {
    let device = GpuDevice::new();
    let texture = device.create_texture(&GpuTextureDesc {
        width: 4,
        height: 4,
        depth: 1,
        format: GpuTextureFormat::Rgba8Unorm,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::RENDER_TARGET
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::CPU_UPLOAD,
        label: "render parity mips",
        mip_levels: 3,
    });
    // Each 2×2 block averages to a whole number: v = 4 (x + 4y).
    let pixels: Vec<u8> = (0..16u8).flat_map(|i| [4 * i, 0, 0, 255]).collect();
    device.upload_texture(&texture, &pixels);
    let levels = [
        device.create_buffer(16, BufferUse::Readback),
        device.create_buffer(4, BufferUse::Readback),
    ];
    let mut enc = device.create_encoder("render parity mips");
    enc.generate_mipmaps(&texture);
    enc.copy_texture_to_buffer(&texture.mip_level_view(1, 2, 2), &levels[0], 2, 2, 8);
    enc.copy_texture_to_buffer(&texture.mip_level_view(2, 1, 1), &levels[1], 1, 1, 4);
    enc.commit_and_wait().expect("GPU work completes");
    let red = |buffer: &GpuBuffer, n: usize| -> Vec<u8> {
        let ptr = buffer.mapped_ptr().expect("readback buffers map");
        unsafe { std::slice::from_raw_parts(ptr, n * 4) }
            .chunks(4)
            .map(|p| p[0])
            .collect()
    };
    assert_eq!(red(&levels[0], 4), [10, 18, 42, 50]);
    assert_eq!(red(&levels[1], 1), [30]);
    assert_clean();
}
