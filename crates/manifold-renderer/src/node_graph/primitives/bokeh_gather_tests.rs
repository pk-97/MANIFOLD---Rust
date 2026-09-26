//! GPU proofs for the layered depth-of-field gather.
//!
//! These tests deliberately exercise `BokehGather::encode`, the same entry
//! point used by the renderer.  Keeping the fixture setup here (rather than
//! dispatching the helper shaders from a second host-side implementation)
//! makes the assertions useful when the internal prefilter changes.

use half::f16;
use manifold_gpu::{
    GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat, GpuTextureUsage,
};

use crate::node_graph::backend::Backend;
use crate::node_graph::bindings::Slot;
use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId};
use crate::node_graph::{
    compile, Executor, FinalOutput, FrameTime, Graph, MetalBackend, NodeInstanceId, ParamValue,
    Source,
};
use crate::render_target::RenderTarget;

use super::{BokehGather, BokehSettings};

const APERTURE_CIRCLE: u32 = 0;
const APERTURE_HEXAGON: u32 = 1;
const APERTURE_OCTAGON: u32 = 2;

#[derive(Clone, Copy)]
struct Pixel {
    rgba: [f32; 4],
}

fn texture(device: &GpuDevice, w: u32, h: u32, pixels: &[Pixel], label: &str) -> GpuTexture {
    assert_eq!(pixels.len(), (w * h) as usize);
    let mut data = Vec::with_capacity(pixels.len() * 4);
    for p in pixels {
        data.extend([
            f16::from_f32(p.rgba[0]),
            f16::from_f32(p.rgba[1]),
            f16::from_f32(p.rgba[2]),
            f16::from_f32(p.rgba[3]),
        ]);
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::SHADER_WRITE
            | GpuTextureUsage::COPY_SRC,
        label,
        mip_levels: 1,
    });
    let bytes = unsafe { std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), data.len() * 2) };
    device.upload_texture(&tex, bytes);
    tex
}

fn solid_pixels(w: u32, h: u32, rgba: [f32; 4]) -> Vec<Pixel> {
    vec![Pixel { rgba }; (w * h) as usize]
}

fn readback(device: &GpuDevice, tex: &GpuTexture) -> Vec<[f32; 4]> {
    let row_bytes = tex.width * 8;
    let buffer = device.create_buffer_shared(u64::from(row_bytes) * u64::from(tex.height));
    let mut encoder = device.create_encoder("bokeh-proof-readback");
    encoder.copy_texture_to_buffer(tex, &buffer, tex.width, tex.height, row_bytes);
    encoder.commit_and_wait_completed();
    let ptr = buffer.mapped_ptr().expect("shared readback buffer");
    let values: &[u16] = unsafe {
        std::slice::from_raw_parts(ptr.cast::<u16>(), (tex.width * tex.height * 4) as usize)
    };
    values
        .chunks_exact(4)
        .map(|p| {
            [
                f16::from_bits(p[0]).to_f32(),
                f16::from_bits(p[1]).to_f32(),
                f16::from_bits(p[2]).to_f32(),
                f16::from_bits(p[3]).to_f32(),
            ]
        })
        .collect()
}

fn run(
    device: &GpuDevice,
    src: &GpuTexture,
    width: &GpuTexture,
    max_radius: f32,
    aperture: u32,
) -> Vec<[f32; 4]> {
    run_quality(device, src, width, max_radius, aperture, 1)
}

fn run_quality(
    device: &GpuDevice,
    src: &GpuTexture,
    width: &GpuTexture,
    max_radius: f32,
    aperture: u32,
    quality: u32,
) -> Vec<[f32; 4]> {
    let mut gather = BokehGather::new();
    run_with_gather_quality(
        &mut gather,
        device,
        src,
        width,
        max_radius,
        aperture,
        quality,
    )
}

fn run_with_gather(
    gather: &mut BokehGather,
    device: &GpuDevice,
    src: &GpuTexture,
    width: &GpuTexture,
    max_radius: f32,
    aperture: u32,
) -> Vec<[f32; 4]> {
    run_with_gather_quality(gather, device, src, width, max_radius, aperture, 1)
}

fn run_with_gather_quality(
    gather: &mut BokehGather,
    device: &GpuDevice,
    src: &GpuTexture,
    width: &GpuTexture,
    max_radius: f32,
    aperture: u32,
    quality: u32,
) -> Vec<[f32; 4]> {
    run_with_settings(
        gather,
        device,
        src,
        width,
        BokehSettings {
            radius: max_radius,
            aperture,
            quality,
            blur_alpha: false,
        },
    )
}

fn run_with_settings(
    gather: &mut BokehGather,
    device: &GpuDevice,
    src: &GpuTexture,
    width: &GpuTexture,
    settings: BokehSettings,
) -> Vec<[f32; 4]> {
    let out = device.create_texture(&GpuTextureDesc {
        width: src.width,
        height: src.height,
        depth: 1,
        format: GpuTextureFormat::Rgba16Float,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
        label: "bokeh-proof-output",
        mip_levels: 1,
    });
    let mut native = device.create_encoder("bokeh-proof");
    {
        let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut native, device);
        gather.encode(&mut gpu, src, width, &out, settings);
    }
    native.commit_and_wait_completed();
    readback(device, &out)
}

fn output_resource(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    plan.steps()
        .iter()
        .find(|step| step.node == node)
        .and_then(|step| {
            step.outputs
                .iter()
                .find(|(name, _)| *name == port)
                .map(|(_, resource)| *resource)
        })
        .unwrap_or_else(|| panic!("no output {port:?} on node {node:?}"))
}

/// Execute the same Source → BokehGather → FinalOutput topology used by the
/// renderer. Keeping this beside the direct encode helper lets the proof
/// catch graph binding, allocation, skip/alias, and dispatch regressions that
/// a primitive-only test cannot observe.
fn run_graph(
    device: &crate::TestDevice,
    src: &GpuTexture,
    width: &GpuTexture,
    settings: BokehSettings,
    enabled: bool,
) -> Vec<[f32; 4]> {
    let mut graph = Graph::new();
    let color = graph.add_node(Box::new(Source::new()));
    let coc = graph.add_node(Box::new(Source::new()));
    let bokeh = graph.add_node(Box::new(BokehGather::new()));
    let output = graph.add_node(Box::new(FinalOutput::new()));
    graph
        .set_param(bokeh, "max_radius", ParamValue::Float(settings.radius))
        .unwrap();
    graph
        .set_param(bokeh, "enabled", ParamValue::Bool(enabled))
        .unwrap();
    graph
        .set_param(bokeh, "aperture", ParamValue::Enum(settings.aperture))
        .unwrap();
    graph
        .set_param(bokeh, "quality", ParamValue::Enum(settings.quality))
        .unwrap();
    graph
        .set_param(bokeh, "blur_alpha", ParamValue::Bool(settings.blur_alpha))
        .unwrap();
    graph.connect((color, "out"), (bokeh, "in")).unwrap();
    graph.connect((coc, "out"), (bokeh, "width")).unwrap();
    graph.connect((bokeh, "out"), (output, "in")).unwrap();
    let plan = compile(&graph).unwrap();

    let color_resource = output_resource(&plan, color, "out");
    let coc_resource = output_resource(&plan, coc, "out");
    let mut native = device.create_encoder("bokeh-graph-proof");
    let mut backend = MetalBackend::new(
        device.arc(),
        src.width,
        src.height,
        GpuTextureFormat::Rgba16Float,
    );
    backend.pre_bind_texture_2d(
        color_resource,
        RenderTarget::view_of(src.clone(), "bokeh-graph-proof-color"),
    );
    backend.pre_bind_texture_2d(
        coc_resource,
        RenderTarget::view_of(width.clone(), "bokeh-graph-proof-coc"),
    );
    let output_slot = Slot(backend.slot_count());
    let mut executor = Executor::new(Box::new(backend));
    {
        let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut native, device);
        executor.execute_frame_with_gpu(
            &mut graph,
            &plan,
            FrameTime {
                beats: manifold_core::Beats(0.0),
                seconds: manifold_core::Seconds(0.0),
                delta: manifold_core::Seconds(1.0 / 60.0),
                frame_count: 0,
            },
            &mut gpu,
        );
    }
    native.commit_and_wait_completed();
    let output_texture = executor
        .backend()
        .texture_2d(output_slot)
        .expect("bokeh graph proof output texture");
    readback(device, output_texture)
}

fn mixed_hdr_scene(w: u32, h: u32) -> (Vec<Pixel>, Vec<(f32, bool)>) {
    let mut colors = Vec::with_capacity((w * h) as usize);
    let mut coc = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let near = x < w / 2;
            let stripe = (x + 3 * y) % 7 == 0;
            let rgba = if near {
                [10.0 + if stripe { 3.0 } else { 0.0 }, 2.5, 0.25, 0.82]
            } else {
                [1.5 + (y % 5) as f32 * 0.25, 5.0, 12.0, 0.64]
            };
            colors.push(Pixel { rgba });
            coc.push((if near { 0.7 } else { 0.85 }, near));
        }
    }
    (colors, coc)
}

#[test]
fn graph_executor_matches_direct_encode_on_mixed_hdr_odd_scene() {
    let device = crate::test_device();
    let (w, h) = (65, 49);
    let (colors, coc) = mixed_hdr_scene(w, h);
    let src = texture(&device, w, h, &colors, "bokeh-graph-proof-mixed-src");
    let width = signed_width(&device, w, h, &coc);
    let settings = BokehSettings {
        radius: 24.0,
        aperture: APERTURE_HEXAGON,
        quality: 1,
        blur_alpha: false,
    };
    let direct = run_with_gather_quality(
        &mut BokehGather::new(),
        &device,
        &src,
        &width,
        settings.radius,
        settings.aperture,
        settings.quality,
    );
    let graph = run_graph(&device, &src, &width, settings, true);
    assert_finite(&direct);
    assert_finite(&graph);
    assert_eq!(graph.len(), (w * h) as usize);
    for (i, (expected, actual)) in direct.iter().zip(&graph).enumerate() {
        let tolerance = 0.12
            + expected[0]
                .abs()
                .max(expected[1].abs())
                .max(expected[2].abs())
                * 0.04;
        for channel in 0..3 {
            assert!(
                (expected[channel] - actual[channel]).abs() <= tolerance,
                "graph/direct mismatch at {i} channel {channel}: expected {}, got {}, tol {tolerance}",
                expected[channel],
                actual[channel]
            );
        }
        assert!(
            (expected[3] - actual[3]).abs() <= 0.03,
            "graph/direct alpha mismatch at {i}: expected {}, got {}",
            expected[3],
            actual[3]
        );
    }
}

#[test]
fn graph_executor_disabled_bokeh_is_hdr_identity_on_odd_scene() {
    let device = crate::test_device();
    let (w, h) = (65, 49);
    let (colors, coc) = mixed_hdr_scene(w, h);
    let src = texture(&device, w, h, &colors, "bokeh-graph-proof-disabled-src");
    let width = signed_width(&device, w, h, &coc);
    let graph = run_graph(
        &device,
        &src,
        &width,
        BokehSettings {
            radius: 24.0,
            aperture: APERTURE_OCTAGON,
            quality: 2,
            blur_alpha: false,
        },
        false,
    );
    assert_finite(&graph);
    for (i, (expected, actual)) in colors
        .iter()
        .map(|pixel| quantize_rgba(pixel.rgba))
        .zip(&graph)
        .enumerate()
    {
        for channel in 0..4 {
            assert_eq!(
                expected[channel], actual[channel],
                "disabled graph changed pixel {i} channel {channel}: expected {}, got {}",
                expected[channel], actual[channel]
            );
        }
    }
}

fn signed_width(device: &GpuDevice, w: u32, h: u32, values: &[(f32, bool)]) -> GpuTexture {
    assert_eq!(values.len(), (w * h) as usize);
    let pixels = values
        .iter()
        .map(|&(coc, near)| Pixel {
            rgba: [coc, if near { 1.0 } else { 0.0 }, coc, 1.0],
        })
        .collect::<Vec<_>>();
    texture(device, w, h, &pixels, "bokeh-proof-width")
}

fn luma(p: [f32; 4]) -> f32 {
    (p[0] + p[1] + p[2]) / 3.0
}

fn assert_finite(pixels: &[[f32; 4]]) {
    assert!(
        pixels.iter().flatten().all(|x| x.is_finite()),
        "DoF output contains NaN/Inf"
    );
}

#[derive(Clone)]
struct OraclePlane {
    w: usize,
    h: usize,
    px: Vec<[f32; 4]>,
}

fn quantize_rgba(p: [f32; 4]) -> [f32; 4] {
    p.map(|v| f16::from_f32(v).to_f32())
}

fn oracle_texel(plane: &OraclePlane, x: i32, y: i32) -> [f32; 4] {
    let x = x.clamp(0, plane.w as i32 - 1) as usize;
    let y = y.clamp(0, plane.h as i32 - 1) as usize;
    plane.px[y * plane.w + x]
}

fn oracle_bilinear(plane: &OraclePlane, u: f32, v: f32) -> [f32; 4] {
    let px = u * plane.w as f32 - 0.5;
    let py = v * plane.h as f32 - 0.5;
    let x0 = px.floor() as i32;
    let y0 = py.floor() as i32;
    let fx = px - x0 as f32;
    let fy = py - y0 as f32;
    let a = oracle_texel(plane, x0, y0);
    let b = oracle_texel(plane, x0 + 1, y0);
    let c = oracle_texel(plane, x0, y0 + 1);
    let d = oracle_texel(plane, x0 + 1, y0 + 1);
    std::array::from_fn(|i| {
        let top = a[i] * (1.0 - fx) + b[i] * fx;
        let bot = c[i] * (1.0 - fx) + d[i] * fx;
        top * (1.0 - fy) + bot * fy
    })
}

fn oracle_prefilter_2x2(source: &OraclePlane) -> OraclePlane {
    let w = source.w.div_ceil(2);
    let h = source.h.div_ceil(2);
    let mut px = vec![[0.0; 4]; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut sum = [0.0; 4];
            for oy in 0..2 {
                for ox in 0..2 {
                    let c = oracle_texel(source, (x * 2 + ox) as i32, (y * 2 + oy) as i32);
                    for i in 0..4 {
                        sum[i] += c[i] * 0.25;
                    }
                }
            }
            px[y * w + x] = quantize_rgba(sum);
        }
    }
    OraclePlane { w, h, px }
}

fn oracle_area_downsample(source: &OraclePlane) -> OraclePlane {
    let w = (source.w / 2).max(1);
    let h = (source.h / 2).max(1);
    let sx = source.w as f32 / w as f32;
    let sy = source.h as f32 / h as f32;
    let mut px = vec![[0.0; 4]; w * h];
    for y in 0..h {
        for x in 0..w {
            let lo_x = x as f32 * sx;
            let hi_x = lo_x + sx;
            let lo_y = y as f32 * sy;
            let hi_y = lo_y + sy;
            let mut sum = [0.0; 4];
            for iy in lo_y.floor() as usize..hi_y.ceil() as usize {
                for ix in lo_x.floor() as usize..hi_x.ceil() as usize {
                    let overlap_x = (hi_x.min(ix as f32 + 1.0) - lo_x.max(ix as f32)).max(0.0);
                    let overlap_y = (hi_y.min(iy as f32 + 1.0) - lo_y.max(iy as f32)).max(0.0);
                    let c = source.px[iy.min(source.h - 1) * source.w + ix.min(source.w - 1)];
                    for i in 0..4 {
                        sum[i] += c[i] * overlap_x * overlap_y;
                    }
                }
            }
            px[y * w + x] = quantize_rgba(sum.map(|v| v / (sx * sy)));
        }
    }
    OraclePlane { w, h, px }
}

fn oracle_chain(source: &OraclePlane, levels: usize) -> Vec<OraclePlane> {
    let mut chain = vec![oracle_prefilter_2x2(source)];
    while chain.len() < levels {
        let next = oracle_area_downsample(chain.last().expect("mip level"));
        chain.push(next);
    }
    chain
}

fn oracle_sample_lod(chain: &[OraclePlane], u: f32, v: f32, lod: f32) -> [f32; 4] {
    let lod = lod.clamp(0.0, (chain.len() - 1) as f32);
    let l0 = lod.floor() as usize;
    let f = lod - l0 as f32;
    let l1 = (l0 + 1).min(chain.len() - 1);
    let a = oracle_bilinear(&chain[l0], u, v);
    let b = oracle_bilinear(&chain[l1], u, v);
    std::array::from_fn(|i| a[i] * (1.0 - f) + b[i] * f)
}

fn oracle_uniform_far_output(source: &OraclePlane, radius: f32, coc: f32) -> OraclePlane {
    let levels = 5.min((source.w.max(source.h) / 2).max(1).ilog2() as usize + 1);
    let chain = oracle_chain(source, levels);
    let half_w = chain[0].w;
    let half_h = chain[0].h;
    let half_radius = radius * coc * 0.5;
    let lod = (half_radius / 2.0).log2().max(0.0);
    let tap_radius = half_radius;
    let mut gather = vec![[0.0; 4]; half_w * half_h];
    for y in 0..half_h {
        for x in 0..half_w {
            let uv = [
                (x as f32 + 0.5) / half_w as f32,
                (y as f32 + 0.5) / half_h as f32,
            ];
            let mut rgb = [0.0; 3];
            let mut coverage = 0.0;
            for i in 0..32 {
                let r = ((i as f32 + 0.5) / 32.0).sqrt();
                let theta = i as f32 * 2.399963;
                let offset = [r * theta.cos(), r * theta.sin()];
                let tap_uv = [
                    uv[0] + offset[0] * tap_radius / half_w as f32,
                    uv[1] + offset[1] * tap_radius / half_h as f32,
                ];
                let sample = oracle_sample_lod(&chain, tap_uv[0], tap_uv[1], lod);
                let distance = r * tap_radius;
                let weight = (tap_radius - distance + 0.5).clamp(0.0, 1.0);
                for c in 0..3 {
                    rgb[c] += sample[c] * weight;
                }
                coverage += weight;
            }
            gather[y * half_w + x] =
                quantize_rgba([rgb[0] / coverage, rgb[1] / coverage, rgb[2] / coverage, 1.0]);
        }
    }
    let far = OraclePlane {
        w: half_w,
        h: half_h,
        px: gather,
    };
    let mut output = vec![[0.0; 4]; source.w * source.h];
    for y in 0..source.h {
        for x in 0..source.w {
            let half_p = [(x as f32 + 0.5) * 0.5 - 0.5, (y as f32 + 0.5) * 0.5 - 0.5];
            let uv = [
                (half_p[0] + 0.5) / half_w as f32,
                (half_p[1] + 0.5) / half_h as f32,
            ];
            output[y * source.w + x] = quantize_rgba(oracle_bilinear(&far, uv[0], uv[1]));
        }
    }
    OraclePlane {
        w: source.w,
        h: source.h,
        px: output,
    }
}

#[test]
fn zero_coc_is_bit_exact_identity() {
    let device = crate::test_device();
    let (w, h) = (37, 19);
    let src_pixels = (0..w * h)
        .map(|i| {
            let x = (i % w) as f32 / w as f32;
            let y = (i / w) as f32 / h as f32;
            Pixel {
                rgba: [x * 3.0, y * 2.0, 0.25, if i % 4 == 0 { 0.0 } else { 0.37 }],
            }
        })
        .collect::<Vec<_>>();
    let src = texture(&device, w, h, &src_pixels, "bokeh-proof-identity-src");
    let width = signed_width(&device, w, h, &vec![(0.0, false); (w * h) as usize]);
    for blur_alpha in [false, true] {
        let out = run_with_settings(
            &mut BokehGather::new(),
            &device,
            &src,
            &width,
            BokehSettings {
                radius: 32.0,
                aperture: APERTURE_CIRCLE,
                quality: 1,
                blur_alpha,
            },
        );
        assert_finite(&out);
        for (i, (expected, got)) in src_pixels.iter().zip(out).enumerate() {
            for (c, got_channel) in got.iter().enumerate() {
                let want = f16::from_f32(expected.rgba[c]).to_f32();
                assert_eq!(
                    got_channel.to_bits(),
                    want.to_bits(),
                    "identity texel {i}, channel {c}, blur_alpha={blur_alpha}"
                );
            }
        }
    }
}

#[test]
fn zero_radius_preserves_detail_across_near_far_edges() {
    let device = crate::test_device();
    let (w, h) = (37, 19);
    let pixels: Vec<_> = (0..w * h)
        .map(|i| Pixel {
            rgba: [(i % 2) as f32 * 4.0, 0.25, 0.5, 0.37],
        })
        .collect();
    let values: Vec<_> = (0..w * h).map(|i| (0.8, i % 3 == 0)).collect();
    let src = texture(&device, w, h, &pixels, "zero-radius-source");
    let coc = signed_width(&device, w, h, &values);
    for radius in [0.0, 0.25] {
        for blur_alpha in [false, true] {
            let out = run_with_settings(
                &mut BokehGather::new(),
                &device,
                &src,
                &coc,
                BokehSettings {
                    radius,
                    aperture: APERTURE_CIRCLE,
                    quality: 1,
                    blur_alpha,
                },
            );
            for (want, got) in pixels.iter().zip(out) {
                assert_eq!(
                    quantize_rgba(want.rgba),
                    got,
                    "radius {radius} changed sharp detail, blur_alpha={blur_alpha}"
                );
            }
        }
    }
}

#[test]
fn blur_alpha_true_expands_opaque_silhouettes_without_dark_fringe() {
    let device = crate::test_device();
    let (w, h) = (65, 33);
    for near in [false, true] {
        let mut colors = solid_pixels(w, h, [0.0, 0.0, 0.0, 0.0]);
        let mut coc = vec![(0.0, false); (w * h) as usize];
        for y in 10..23 {
            for x in 27..38 {
                let i = (y * w + x) as usize;
                colors[i] = Pixel {
                    rgba: [8.0, 2.0, 0.5, 1.0],
                };
                coc[i] = (0.8, near);
            }
        }
        let src = texture(&device, w, h, &colors, "bokeh-proof-alpha-near-src");
        let width = signed_width(&device, w, h, &coc);
        let out = run_with_settings(
            &mut BokehGather::new(),
            &device,
            &src,
            &width,
            BokehSettings {
                radius: 28.0,
                aperture: APERTURE_CIRCLE,
                quality: 1,
                blur_alpha: true,
            },
        );
        assert_finite(&out);
        let mut halo_alpha = 0.0_f32;
        for y in 0..h {
            for x in 0..w {
                if (27..38).contains(&x) && (10..23).contains(&y) {
                    continue;
                }
                let p = out[(y * w + x) as usize];
                halo_alpha = halo_alpha.max(p[3]);
                if p[3] > 0.02 {
                    assert!(
                        (p[0] - 8.0).abs() < 0.1 && (p[1] - 2.0).abs() < 0.05,
                        "straight-alpha halo acquired a dark fringe at ({x},{y}): {p:?}"
                    );
                }
            }
        }
        let alpha_mass: f32 = out.iter().map(|p| p[3]).sum();
        assert!(
            (alpha_mass / 143.0 - 1.0).abs() < 0.25,
            "defocusing changed foreground coverage mass: {alpha_mass} vs 143"
        );
        assert!(
            halo_alpha > 0.02,
            "opaque near foreground did not expand alpha beyond its silhouette: {halo_alpha}"
        );
    }
}

#[test]
fn blur_alpha_true_preserves_translucent_flat_hdr_near_and_far() {
    let device = crate::test_device();
    let (w, h) = (128, 64);
    let colors = solid_pixels(w, h, [6.0, 2.0, 0.5, 0.37]);
    let coc = (0..w * h)
        .map(|i| (if i % w < w / 2 { 0.7 } else { 0.8 }, i % w < w / 2))
        .collect::<Vec<_>>();
    let src = texture(&device, w, h, &colors, "bokeh-proof-alpha-flat-src");
    let width = signed_width(&device, w, h, &coc);
    let out = run_with_settings(
        &mut BokehGather::new(),
        &device,
        &src,
        &width,
        BokehSettings {
            radius: 32.0,
            aperture: APERTURE_CIRCLE,
            quality: 1,
            blur_alpha: true,
        },
    );
    assert_finite(&out);
    for y in 4..h - 4 {
        for x in [16, 112] {
            let p = out[(y * w + x) as usize];
            assert!(
                (p[0] - 6.0).abs() < 0.2,
                "flat HDR red changed at ({x},{y}): {}",
                p[0]
            );
            assert!(
                (p[1] - 2.0).abs() < 0.1,
                "flat HDR green changed at ({x},{y}): {}",
                p[1]
            );
            assert!(
                (p[2] - 0.5).abs() < 0.05,
                "flat HDR blue changed at ({x},{y}): {}",
                p[2]
            );
            assert!(
                (p[3] - 0.37).abs() < 0.02,
                "flat HDR alpha changed at ({x},{y}): {}",
                p[3]
            );
        }
    }
}

#[test]
fn near_opacity_does_not_fade_during_focus_transition() {
    let device = crate::test_device();
    let (w, h) = (32, 32);
    let colors = solid_pixels(w, h, [6.0, 2.0, 0.5, 0.37]);
    let src = texture(&device, w, h, &colors, "focus-opacity-source");
    for radius in [0.25, 0.75, 1.0, 1.25, 1.75, 2.5, 4.0] {
        let coc = signed_width(
            &device,
            w,
            h,
            &vec![(radius / 24.0, true); (w * h) as usize],
        );
        let out = run_with_settings(
            &mut BokehGather::new(),
            &device,
            &src,
            &coc,
            BokehSettings {
                radius: 24.0,
                aperture: 0,
                quality: 1,
                blur_alpha: true,
            },
        );
        for p in out {
            assert!(
                (p[3] - 0.37).abs() < 0.005 && (p[0] - 6.0).abs() < 0.03,
                "focus radius {radius} dimmed uniform translucent foreground: {p:?}"
            );
        }
    }
}

#[test]
fn blur_alpha_true_ignores_hidden_rgb_in_transparent_far_texels() {
    let device = crate::test_device();
    let (w, h) = (65, 33);
    let mut colors = solid_pixels(w, h, [0.0, 0.0, 0.0, 0.0]);
    colors[(h / 2 * w + w / 2) as usize] = Pixel {
        rgba: [40.0, 10.0, 2.0, 0.0],
    };
    let width = signed_width(&device, w, h, &vec![(0.8, false); (w * h) as usize]);
    let src = texture(&device, w, h, &colors, "bokeh-proof-alpha-hidden-src");
    let out = run_with_settings(
        &mut BokehGather::new(),
        &device,
        &src,
        &width,
        BokehSettings {
            radius: 32.0,
            aperture: APERTURE_CIRCLE,
            quality: 1,
            blur_alpha: true,
        },
    );
    assert_finite(&out);
    let max_channel = out
        .iter()
        .flat_map(|p| p.iter())
        .copied()
        .fold(0.0, f32::max);
    assert!(
        max_channel < 0.02,
        "transparent hidden RGB bled into the far gather: max channel {max_channel}"
    );
}

#[test]
fn uniform_far_cpu_oracle_matches_even_and_odd_area_mips() {
    let device = crate::test_device();
    for &(w, h) in &[(64u32, 48u32), (65u32, 49u32)] {
        let source_pixels = (0..w * h)
            .map(|i| {
                let x = i % w;
                let y = i / w;
                let rgba = [
                    0.5 + 4.5 * x as f32 / (w - 1) as f32 + 0.25 * y as f32 / h as f32,
                    0.25 + 2.0 * y as f32 / (h - 1) as f32,
                    0.1 + 1.2 * (x + y) as f32 / (w + h - 2) as f32,
                    1.0,
                ];
                Pixel {
                    rgba: quantize_rgba(rgba),
                }
            })
            .collect::<Vec<_>>();
        let oracle_source = OraclePlane {
            w: w as usize,
            h: h as usize,
            px: source_pixels.iter().map(|p| p.rgba).collect(),
        };
        let src = texture(&device, w, h, &source_pixels, "bokeh-proof-oracle-src");
        let width = signed_width(&device, w, h, &vec![(0.8, false); (w * h) as usize]);
        let got = run_quality(&device, &src, &width, 24.0, APERTURE_CIRCLE, 1);
        let expected = oracle_uniform_far_output(&oracle_source, 24.0, 0.8);
        for (i, (actual, reference)) in got.iter().zip(expected.px.iter()).enumerate() {
            for c in 0..3 {
                assert!(
                    (actual[c] - reference[c]).abs() < 0.02,
                    "{w}x{h} oracle mismatch texel {i} channel {c}: actual={} expected={}",
                    actual[c],
                    reference[c]
                );
            }
        }
    }
}

#[test]
fn flat_hdr_colour_keeps_alpha_and_energy() {
    let device = crate::test_device();
    let (w, h) = (29, 17);
    let src = texture(
        &device,
        w,
        h,
        &solid_pixels(w, h, [6.0, 2.0, 0.5, 0.37]),
        "bokeh-proof-hdr-src",
    );
    let width = signed_width(&device, w, h, &vec![(0.72, false); (w * h) as usize]);
    let out = run(&device, &src, &width, 40.0, APERTURE_CIRCLE);
    assert_finite(&out);
    for (i, p) in out.iter().enumerate() {
        assert!((p[3] - 0.37).abs() < 0.01, "alpha changed at {i}: {}", p[3]);
        assert!((p[0] - 6.0).abs() < 0.1, "HDR red changed at {i}: {}", p[0]);
        assert!(
            (p[1] - 2.0).abs() < 0.1,
            "HDR green changed at {i}: {}",
            p[1]
        );
        assert!(
            (p[2] - 0.5).abs() < 0.05,
            "HDR blue changed at {i}: {}",
            p[2]
        );
    }
}

#[test]
fn thin_near_object_survives_half_res_prefilter() {
    let device = crate::test_device();
    let (w, h) = (65, 33);
    let mut colors = solid_pixels(w, h, [0.0, 0.0, 0.0, 1.0]);
    let mut coc = vec![(0.0, false); (w * h) as usize];
    for y in 0..h {
        let i = (y * w + w / 2) as usize;
        colors[i] = Pixel {
            rgba: [12.0, 3.0, 1.0, 1.0],
        };
        coc[i] = (0.8, true);
    }
    let src = texture(&device, w, h, &colors, "bokeh-proof-thin-src");
    let width = signed_width(&device, w, h, &coc);
    let out = run(&device, &src, &width, 28.0, APERTURE_CIRCLE);
    assert_finite(&out);
    let total: f32 = out.iter().map(|p| luma(*p)).sum();
    let centre_column: f32 = (0..h).map(|y| luma(out[(y * w + w / 2) as usize])).sum();
    let input_total = (12.0 + 3.0 + 1.0) / 3.0 * h as f32;
    let energy_ratio = total / input_total;
    assert!(
        total > 8.0,
        "one-pixel near object disappeared after prefilter: {total}"
    );
    assert!(
        (0.8..=1.2).contains(&energy_ratio),
        "thin near object energy ratio {energy_ratio} (output={total}, input={input_total})"
    );
    assert!(
        centre_column > 2.0,
        "near line did not survive at its centre: {centre_column}"
    );
}

#[test]
fn bright_far_background_does_not_bleed_into_sharp_foreground() {
    let device = crate::test_device();
    let (w, h) = (71, 35);
    let mut colors = solid_pixels(w, h, [8.0, 8.0, 8.0, 1.0]);
    let mut coc = vec![(0.8, false); (w * h) as usize];
    for y in 0..h {
        for x in (w / 2 - 2)..=(w / 2 + 2) {
            let i = (y * w + x) as usize;
            colors[i] = Pixel {
                rgba: [0.0, 0.0, 0.0, 1.0],
            };
            coc[i] = (0.0, false);
        }
    }
    let src = texture(&device, w, h, &colors, "bokeh-proof-spill-src");
    let width = signed_width(&device, w, h, &coc);
    let out = run(&device, &src, &width, 32.0, APERTURE_CIRCLE);
    assert_finite(&out);
    let stripe = (0..h)
        .map(|y| luma(out[(y * w + w / 2) as usize]))
        .fold(0.0, f32::max);
    assert!(
        stripe < 1.0,
        "sharp foreground was contaminated by far background: {stripe}"
    );
}

#[test]
fn rim_fixture_feathers_beyond_bright_rectangle_without_cliff() {
    let device = crate::test_device();
    let (w, h) = (64, 64);
    let rect0 = 24;
    let rect1 = 40;
    let mut colors = solid_pixels(w, h, [0.0, 0.0, 0.0, 1.0]);
    // Black surroundings belong to the same defocused plane. A zero-CoC
    // surrounding surface would be a sharp occluder that must reject far spill.
    let coc = vec![(0.5, false); (w * h) as usize];
    for y in rect0..rect1 {
        for x in rect0..rect1 {
            let i = (y * w + x) as usize;
            colors[i] = Pixel {
                rgba: [20.0, 20.0, 20.0, 1.0],
            };
        }
    }
    let src = texture(&device, w, h, &colors, "bokeh-proof-rim-src");
    let width = signed_width(&device, w, h, &coc);
    let out = run(&device, &src, &width, 24.0, APERTURE_CIRCLE);
    assert_finite(&out);
    let input_energy = 20.0 * 16.0 * 16.0;
    let output_energy: f32 = out.iter().map(|p| p[0]).sum();
    assert!(
        output_energy > input_energy * 0.55,
        "rim fixture lost too much bright energy: {output_energy} vs {input_energy}"
    );
    let mut inside_reach = 0.0;
    let mut outside = 0.0;
    for y in 0..h {
        for x in 0..w {
            let dx = if x < rect0 {
                rect0 - x
            } else if x >= rect1 {
                x - rect1 + 1
            } else {
                0
            };
            let dy = if y < rect0 {
                rect0 - y
            } else if y >= rect1 {
                y - rect1 + 1
            } else {
                0
            };
            let edge_distance = dx.max(dy);
            if (4..=8).contains(&edge_distance) {
                inside_reach += luma(out[(y * w + x) as usize]);
            }
            if edge_distance >= 20 {
                outside += luma(out[(y * w + x) as usize]);
            }
        }
    }
    assert!(
        inside_reach > 0.5,
        "rim fixture has no visible halo in reach band"
    );
    assert!(
        outside < input_energy * 0.15,
        "rim fixture formed a bright outer plateau: {outside}"
    );
}

#[test]
fn near_halo_overlays_edge_but_keeps_dark_bar_interior() {
    let device = crate::test_device();
    let (w, h) = (128, 64);
    let mut colors = solid_pixels(w, h, [0.0, 0.0, 0.0, 1.0]);
    let mut coc = vec![(0.0, false); (w * h) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = (y * w + x) as usize;
            if x < 40 {
                colors[i] = Pixel {
                    rgba: [10.0, 10.0, 10.0, 1.0],
                };
                coc[i] = (0.6, true);
            } else if x < 96 {
                // A wide, sharp dark foreground bar.
                coc[i] = (0.0, false);
            }
        }
    }
    let src = texture(&device, w, h, &colors, "bokeh-proof-near-bar-src");
    let width = signed_width(&device, w, h, &coc);
    let out = run(&device, &src, &width, 24.0, APERTURE_CIRCLE);
    assert_finite(&out);
    let edge_mean = (0..h)
        .map(|y| luma(out[(y * w + 42) as usize]))
        .sum::<f32>()
        / h as f32;
    let mut interior_max: f32 = 0.0;
    for y in 0..h {
        for x in 52..84 {
            interior_max = interior_max.max(luma(out[(y * w + x) as usize]));
        }
    }
    assert!(
        edge_mean > 0.05,
        "near halo did not overlap the bar edge: {edge_mean}"
    );
    assert!(
        interior_max < 0.8,
        "near halo replaced the dark bar interior: {interior_max}"
    );
}

#[test]
fn tiny_odd_dimensions_and_radius_modulation_are_stable() {
    let device = crate::test_device();
    let mut gather = BokehGather::new();
    for &(w, h) in &[(1, 1), (2, 3), (3, 5), (7, 2), (17, 11)] {
        let src = texture(
            &device,
            w,
            h,
            &solid_pixels(w, h, [1.0, 0.25, 0.5, 0.75]),
            "bokeh-proof-resize-src",
        );
        let width = signed_width(&device, w, h, &vec![(0.5, false); (w * h) as usize]);
        for &radius in &[1.0, 7.0, 63.0] {
            let out = run_with_gather(&mut gather, &device, &src, &width, radius, APERTURE_CIRCLE);
            assert_eq!(out.len(), (w * h) as usize);
            assert_finite(&out);
            assert!(out.iter().all(|p| (p[3] - 0.75).abs() < 0.02));
        }
    }
}

#[test]
fn warm_resize_rebuilds_resources_and_keeps_modulation_finite() {
    let device = crate::test_device();
    let mut gather = BokehGather::new();
    for &(w, h) in &[(17, 9), (33, 15), (5, 3), (33, 15)] {
        let src = texture(
            &device,
            w,
            h,
            &solid_pixels(w, h, [2.0, 0.5, 0.125, 0.61]),
            "bokeh-proof-warm-resize-src",
        );
        let width = signed_width(&device, w, h, &vec![(0.35, false); (w * h) as usize]);
        for (radius, aperture) in [(2.0, APERTURE_CIRCLE), (18.0, APERTURE_HEXAGON)] {
            let out = run_with_gather(&mut gather, &device, &src, &width, radius, aperture);
            assert_eq!(out.len(), (w * h) as usize);
            assert_finite(&out);
            assert!(out.iter().all(|p| (p[3] - 0.61).abs() < 0.02));
        }
    }
}

#[test]
fn quality_levels_preserve_constant_hdr_colour_across_layer_boundary() {
    let device = crate::test_device();
    let (w, h) = (48, 24);
    let colors = solid_pixels(w, h, [6.0, 2.0, 0.5, 0.37]);
    let coc = (0..w * h)
        .map(|i| (0.65, i % w >= w / 2))
        .collect::<Vec<_>>();
    let src = texture(&device, w, h, &colors, "bokeh-proof-quality-src");
    let width = signed_width(&device, w, h, &coc);
    for quality in 0..=2 {
        let out = run_quality(&device, &src, &width, 32.0, APERTURE_CIRCLE, quality);
        assert_finite(&out);
        for (i, p) in out.iter().enumerate() {
            assert!(
                (p[0] - 6.0).abs() < 0.15,
                "quality {quality} red changed at {i}: {}",
                p[0]
            );
            assert!(
                (p[1] - 2.0).abs() < 0.08,
                "quality {quality} green changed at {i}: {}",
                p[1]
            );
            assert!(
                (p[2] - 0.5).abs() < 0.03,
                "quality {quality} blue changed at {i}: {}",
                p[2]
            );
            assert!(
                (p[3] - 0.37).abs() < 0.01,
                "quality {quality} alpha changed at {i}: {}",
                p[3]
            );
        }
    }
}

#[test]
fn aperture_shapes_change_bokeh_without_changing_flat_colour() {
    let device = crate::test_device();
    let (w, h) = (81, 81);
    let mut colors = solid_pixels(w, h, [0.0, 0.0, 0.0, 1.0]);
    let centre = (h / 2 * w + w / 2) as usize;
    colors[centre] = Pixel {
        rgba: [10.0, 8.0, 2.0, 1.0],
    };
    let src = texture(&device, w, h, &colors, "bokeh-proof-aperture-src");
    let width = signed_width(&device, w, h, &vec![(0.75, false); (w * h) as usize]);
    let circle = run(&device, &src, &width, 24.0, APERTURE_CIRCLE);
    let hex = run(&device, &src, &width, 24.0, APERTURE_HEXAGON);
    let oct = run(&device, &src, &width, 24.0, APERTURE_OCTAGON);
    assert_finite(&circle);
    assert_finite(&hex);
    assert_finite(&oct);
    let d_hex: f32 = circle
        .iter()
        .zip(&hex)
        .map(|(a, b)| (luma(*a) - luma(*b)).abs())
        .sum();
    let d_oct: f32 = circle
        .iter()
        .zip(&oct)
        .map(|(a, b)| (luma(*a) - luma(*b)).abs())
        .sum();
    assert!(
        d_hex > 0.05,
        "hexagonal aperture had no measurable shape effect"
    );
    assert!(
        d_oct > 0.05,
        "octagonal aperture had no measurable shape effect"
    );
}

#[test]
fn isolated_hdr_highlight_spreads_energy_over_a_bounded_footprint() {
    let device = crate::test_device();
    let (w, h) = (96, 64);
    let mut colors = solid_pixels(w, h, [0.0, 0.0, 0.0, 1.0]);
    let coc = vec![(0.8, false); (w * h) as usize];
    let centre = (w / 2, h / 2);
    colors[(centre.1 * w + centre.0) as usize] = Pixel {
        rgba: [24.0, 12.0, 4.0, 1.0],
    };
    let src = texture(&device, w, h, &colors, "bokeh-proof-isolated-hdr-src");
    let width = signed_width(&device, w, h, &coc);
    let out = run(&device, &src, &width, 32.0, APERTURE_CIRCLE);
    assert_finite(&out);

    let input = [24.0, 12.0, 4.0];
    let mut sums = [0.0; 3];
    let mut peak = [0.0f32; 3];
    let mut weighted_radius = 0.0;
    let mut support = 0u32;
    for y in 0..h {
        for x in 0..w {
            let p = out[(y * w + x) as usize];
            for c in 0..3 {
                sums[c] += p[c];
                peak[c] = peak[c].max(p[c]);
            }
            let brightness = luma(p);
            if brightness > 0.002 {
                support += 1;
                let dx = x as f32 + 0.5 - (centre.0 as f32 + 0.5);
                let dy = y as f32 + 0.5 - (centre.1 as f32 + 0.5);
                weighted_radius += brightness * (dx * dx + dy * dy);
            }
        }
    }
    for c in 0..3 {
        assert!(
            sums[c] > input[c] * 0.35 && sums[c] < input[c] * 2.5,
            "channel {c} energy {} is outside the bounded gather range for {}",
            sums[c],
            input[c]
        );
        assert!(
            peak[c] < input[c] * 0.8,
            "channel {c} retained an isolated firefly peak {peak:?}"
        );
    }
    assert!(
        support >= 12,
        "isolated highlight footprint collapsed to {support} pixels"
    );
    assert!(
        weighted_radius / sums[0] > 8.0,
        "isolated highlight footprint is too concentrated: second moment {}",
        weighted_radius / sums[0]
    );
    assert!(
        support < ((w * h) as f32 * 0.45) as u32,
        "isolated highlight spread across an unbounded fraction of the frame: {support}"
    );
}

#[test]
fn small_far_blur_is_independent_of_unreachable_tile_maximum() {
    let device = crate::test_device();
    let (w, h) = (128, 128);
    let colors = (0..w * h)
        .map(|i| {
            let v = if ((i % w) / 3 + (i / w) / 3) % 2 == 0 {
                4.0
            } else {
                0.25
            };
            Pixel {
                rgba: [v, v * 0.5, 1.0, 1.0],
            }
        })
        .collect::<Vec<_>>();
    let src = texture(&device, w, h, &colors, "small-coc-tile-proof");
    let uniform = signed_width(&device, w, h, &vec![(0.125, false); (w * h) as usize]);
    let values = (0..w * h)
        .map(|i| (if i / w >= 80 { 1.0 } else { 0.125 }, false))
        .collect::<Vec<_>>();
    let mixed = signed_width(&device, w, h, &values);
    let reference = run(&device, &src, &uniform, 24.0, 0);
    let actual = run(&device, &src, &mixed, 24.0, 0);
    // More than the 24-pixel maximum radius from the changed surface, but
    // inside conservative tile reach. Its blur must not alter local detail.
    for y in 32..48 {
        for x in 32..96 {
            let i = (y * w + x) as usize;
            for c in 0..3 {
                assert!(
                    (actual[i][c] - reference[i][c]).abs() < 0.005,
                    "unreachable far tile changed ({x},{y}) channel {c}: {} vs {}",
                    actual[i][c],
                    reference[i][c]
                );
            }
        }
    }
}

#[test]
fn focused_checkerboard_detail_survives_beside_blurred_region() {
    let device = crate::test_device();
    let (w, h) = (80, 32);
    let mut colors = solid_pixels(w, h, [0.25, 0.25, 0.25, 1.0]);
    let mut coc = vec![(0.8, false); (w * h) as usize];
    for y in 0..h {
        for x in 0..(w / 2) {
            let bright = (x / 4 + y / 4) % 2 == 0;
            let i = (y * w + x) as usize;
            colors[i] = Pixel {
                rgba: if bright {
                    [1.0, 1.0, 1.0, 1.0]
                } else {
                    [0.0, 0.0, 0.0, 1.0]
                },
            };
            coc[i] = (0.0, false);
        }
    }
    let src = texture(&device, w, h, &colors, "bokeh-proof-checker-src");
    let width = signed_width(&device, w, h, &coc);
    let out = run(&device, &src, &width, 28.0, APERTURE_CIRCLE);
    assert_finite(&out);
    let mut focused_values = Vec::new();
    for x in 8..(w / 2 - 8) {
        for y in 8..(h - 8) {
            focused_values.push(luma(out[(y * w + x) as usize]));
        }
    }
    let focused_min = focused_values.iter().copied().fold(f32::INFINITY, f32::min);
    let focused_max = focused_values
        .iter()
        .copied()
        .fold(f32::NEG_INFINITY, f32::max);
    let mut blurred_values = Vec::new();
    for x in (w / 2 + 8)..(w - 8) {
        for y in 8..(h - 8) {
            blurred_values.push(luma(out[(y * w + x) as usize]));
        }
    }
    let blurred_mean = blurred_values.iter().sum::<f32>() / blurred_values.len() as f32;
    let blurred_variance = blurred_values
        .iter()
        .map(|v| (v - blurred_mean).powi(2))
        .sum::<f32>()
        / blurred_values.len() as f32;
    assert!(
        focused_max - focused_min > 0.75,
        "focused checkerboard contrast collapsed"
    );
    assert!(
        blurred_variance.sqrt() < 0.05,
        "far region retained checkerboard detail"
    );
}

#[test]
fn motion_and_focus_sweep_has_bounded_frame_to_frame_change() {
    let device = crate::test_device();
    let (w, h) = (96, 48);
    let mut previous: Option<(Vec<[f32; 4]>, f32, f32)> = None;
    let background_luma = (0.01 + 0.02 + 0.04) / 3.0;
    for frame in 0..10 {
        let mut colors = solid_pixels(w, h, [0.01, 0.02, 0.04, 1.0]);
        let x = 20.25 + frame as f32 * 0.63;
        let y = 24.0 + (frame as f32 * 0.3).sin() * 2.0;
        for iy in 0..h {
            for ix in 0..w {
                let dx = ix as f32 + 0.5 - x;
                let dy = iy as f32 + 0.5 - y;
                let energy = (-0.5 * (dx * dx + dy * dy) / 3.0).exp();
                let i = (iy * w + ix) as usize;
                colors[i].rgba[0] += energy * 7.0;
                colors[i].rgba[1] += energy * 2.0;
            }
        }
        let coc = vec![(0.2 + frame as f32 * 0.04, false); (w * h) as usize];
        let src = texture(&device, w, h, &colors, "bokeh-proof-motion-src");
        let width = signed_width(&device, w, h, &coc);
        let out = run(&device, &src, &width, 24.0, APERTURE_CIRCLE);
        assert_finite(&out);
        let mut signal_energy = 0.0;
        let mut signal_x = 0.0;
        for (i, p) in out.iter().enumerate() {
            let signal = (luma(*p) - background_luma).max(0.0);
            signal_energy += signal;
            signal_x += signal * (i as u32 % w) as f32;
        }
        let centroid_x = signal_x / signal_energy.max(1e-6);
        if let Some((old, old_energy, old_centroid_x)) = previous {
            let delta = out
                .iter()
                .zip(old)
                .map(|(a, b)| (luma(*a) - luma(b)).abs())
                .sum::<f32>()
                / (w * h) as f32;
            assert!(delta < 0.8, "subpixel motion/focus sweep jumped by {delta}");
            let relative_energy = (signal_energy - old_energy).abs() / old_energy.max(1e-6);
            assert!(
                relative_energy < 0.1,
                "motion/focus sweep changed signal energy by {:.1}%",
                relative_energy * 100.0
            );
            assert!(
                (centroid_x - old_centroid_x).abs() < 1.5,
                "motion/focus sweep centroid jumped from {old_centroid_x} to {centroid_x}"
            );
        }
        previous = Some((out, signal_energy, centroid_x));
    }
}

fn save_artifact(dir: &str, name: &str, pixels: &[[f32; 4]], w: u32, h: u32) {
    let image = image::RgbaImage::from_fn(w, h, |x, y| {
        let p = pixels[(y * w + x) as usize];
        let tone = |v: f32| ((v.max(0.0) / (1.0 + v.max(0.0))).powf(1.0 / 2.2) * 255.0) as u8;
        image::Rgba([
            tone(p[0]),
            tone(p[1]),
            tone(p[2]),
            (p[3].clamp(0.0, 1.0) * 255.0) as u8,
        ])
    });
    std::fs::create_dir_all(dir).expect("create DoF artifact directory");
    image
        .save(std::path::Path::new(dir).join(format!("{name}.png")))
        .expect("save DoF artifact");
}

#[test]
fn production_pipeline_baseline_fixture_emits_optional_visual_artifact() {
    let device = crate::test_device();
    let (w, h) = (320, 192);
    let mut colors = Vec::with_capacity((w * h) as usize);
    let mut coc = Vec::with_capacity((w * h) as usize);
    for y in 0..h {
        for x in 0..w {
            let near = (125..131).contains(&x) || (210..244).contains(&x) && (60..140).contains(&y);
            let bright = ((x as i32 - 70).pow(2) + (y as i32 - 85).pow(2) < 16)
                || ((x as i32 - 175).pow(2) + (y as i32 - 110).pow(2) < 9);
            let rgba = if near {
                [0.03, 0.6, 0.1, 1.0]
            } else if bright {
                [12.0, 5.0, 1.0, 1.0]
            } else {
                [0.03, 0.04, 0.1, 1.0]
            };
            colors.push(Pixel { rgba });
            coc.push((if near { 0.6 } else { 0.8 }, near));
        }
    }
    let src = texture(&device, w, h, &colors, "bokeh-proof-baseline-src");
    let width = signed_width(&device, w, h, &coc);
    let out = run(&device, &src, &width, 24.0, APERTURE_CIRCLE);
    assert_finite(&out);
    if let Ok(dir) = std::env::var("MANIFOLD_DOF_ARTIFACT_DIR") {
        save_artifact(&dir, "dof-upgrade-baseline", &out, w, h);
    }
}

#[test]
fn bounded_gpu_benchmark_1080p_and_4k() {
    if std::env::var_os("MANIFOLD_DOF_BENCH").is_none() {
        return;
    }
    let device = crate::test_device();
    for &(w, h) in &[(1920, 1080), (3840, 2160)] {
        let src = texture(
            &device,
            w,
            h,
            &solid_pixels(w, h, [0.2, 0.3, 0.5, 1.0]),
            "bokeh-proof-benchmark-src",
        );
        let width = signed_width(&device, w, h, &vec![(0.45, false); (w * h) as usize]);
        let out = device.create_texture(&GpuTextureDesc {
            width: w,
            height: h,
            depth: 1,
            format: GpuTextureFormat::Rgba16Float,
            dimension: GpuTextureDimension::D2,
            usage: GpuTextureUsage::SHADER_WRITE | GpuTextureUsage::COPY_SRC,
            label: "bokeh-proof-benchmark-output",
            mip_levels: 1,
        });
        let mut gather = BokehGather::new();
        let settings = BokehSettings {
            radius: 32.0,
            aperture: APERTURE_CIRCLE,
            quality: 1,
            blur_alpha: true,
        };
        for _ in 0..3 {
            let mut native = device.create_encoder("bokeh-proof-benchmark-warmup");
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut native, &device);
            gather.encode(&mut gpu, &src, &width, &out, settings);
            native.commit_and_wait_completed();
        }
        let mut samples = Vec::with_capacity(10);
        for _ in 0..10 {
            let mut native = device.create_encoder("bokeh-proof-benchmark-steady");
            let mut gpu = crate::gpu_encoder::GpuEncoder::new(&mut native, &device);
            gather.encode(&mut gpu, &src, &width, &out, settings);
            samples.push(native.commit_and_wait_completed_timed() * 1000.0);
        }
        samples.sort_by(f64::total_cmp);
        let median = samples[samples.len() / 2];
        let p95 = samples[((samples.len() * 95).div_ceil(100) - 1).min(samples.len() - 1)];
        eprintln!(
            "bokeh_gather {w}x{h}: median={median:.3}ms p95={p95:.3}ms (n={})",
            samples.len()
        );
    }
}
