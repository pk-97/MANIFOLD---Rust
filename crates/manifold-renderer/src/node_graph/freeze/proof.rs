//! First end-to-end fusion proof (design build-sequence: the thin vertical
//! slice before codegen). Renders a real unfused `Source -> Gain -> Gain ->
//! FinalOutput` chain through the production executor, renders a hand-fused
//! single-kernel equivalent, and checks them against the [`TextureDiff`]
//! oracle. Two claims:
//!
//! 1. A *correct* fusion clears the oracle — even though it is NOT bit-exact
//!    with the unfused chain (the chain rounds to f16 between every pass; the
//!    fused kernel keeps f32 in registers and rounds once). The two-sided
//!    tolerance absorbs that f16-accumulation drift, which is the whole reason
//!    it is two-sided (design §11.D).
//! 2. A *wrong* fusion fails the oracle. Without this, "the oracle passed"
//!    would be meaningless — so we deliberately mis-fuse (product off by 1.5×)
//!    and assert it is flagged.
//!
//! This makes the diff core earn its keep on a genuine fusion, not a planted
//! perturbation, and gives the eventual codegen a known-good reference target.

use super::TextureDiff;
use crate::gpu_encoder::GpuEncoder as RendererGpuEncoder;
use crate::node_graph::execution_plan::{ExecutionPlan, ResourceId, compile};
use crate::node_graph::graph::Graph;
use crate::node_graph::parameters::ParamValue;
use crate::node_graph::primitives::Gain;
use crate::node_graph::{Executor, FinalOutput, FrameTime, MetalBackend, NodeInstanceId, Source};
use crate::render_target::RenderTarget;
use half::f16;
use manifold_core::{Beats, Seconds};
use manifold_gpu::{
    GpuBinding, GpuDevice, GpuTexture, GpuTextureDesc, GpuTextureDimension, GpuTextureFormat,
    GpuTextureUsage,
};

const FMT: GpuTextureFormat = GpuTextureFormat::Rgba16Float;

fn frame_time() -> FrameTime {
    FrameTime {
        beats: Beats(0.0),
        seconds: Seconds(0.0),
        delta: Seconds(1.0 / 60.0),
        frame_count: 0,
    }
}

fn resource_for_output(plan: &ExecutionPlan, node: NodeInstanceId, port: &str) -> ResourceId {
    for step in plan.steps() {
        if step.node == node {
            for &(name, id) in &step.outputs {
                if name == port {
                    return id;
                }
            }
        }
    }
    panic!("no output `{port}` on node {node:?}");
}

/// CPU-built RGBA gradient as a CPU-uploadable source texture — spatially
/// varying so a pointwise fusion bug that's invisible on a flat fill can't
/// hide. R ramps in x, G in y, B fixed, A = 1.
fn gradient_input(device: &GpuDevice, w: u32, h: u32) -> GpuTexture {
    let mut px = vec![f16::from_f32(0.0); (w * h * 4) as usize];
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            px[i] = f16::from_f32(x as f32 / w as f32);
            px[i + 1] = f16::from_f32(y as f32 / h as f32);
            px[i + 2] = f16::from_f32(0.5);
            px[i + 3] = f16::from_f32(1.0);
        }
    }
    let tex = device.create_texture(&GpuTextureDesc {
        width: w,
        height: h,
        depth: 1,
        format: FMT,
        dimension: GpuTextureDimension::D2,
        usage: GpuTextureUsage::CPU_UPLOAD
            | GpuTextureUsage::SHADER_READ
            | GpuTextureUsage::COPY_SRC,
        label: "freeze-proof-input",
        mip_levels: 1,
    });
    let bytes = unsafe {
        std::slice::from_raw_parts(px.as_ptr().cast::<u8>(), std::mem::size_of_val(px.as_slice()))
    };
    device.upload_texture(&tex, bytes);
    tex
}

/// Render an effect graph to a standalone texture (the unfused / oracle side).
/// Copies `input` into the source slot, runs one frame, copies the bound
/// output into a fresh target that outlives the backend.
fn render_graph(
    device: &GpuDevice,
    graph: &mut Graph,
    plan: &ExecutionPlan,
    source_res: ResourceId,
    input: &GpuTexture,
    output_res: ResourceId,
) -> RenderTarget {
    let (w, h) = (input.width, input.height);

    let src_rt = RenderTarget::new(device, w, h, FMT, "freeze-src");
    {
        let mut e = device.create_encoder("freeze-src-fill");
        e.copy_texture_to_texture(input, &src_rt.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-graph-out");

    let mut backend = MetalBackend::new(device, w, h, FMT);
    backend.pre_bind_texture_2d(source_res, src_rt);
    let out_slot = backend.pre_bind_texture_2d(output_res, out_rt);

    let mut enc = device.create_encoder("freeze-graph-exec");
    let mut exec = Executor::new(Box::new(backend));
    {
        let mut gpu = RendererGpuEncoder::new(&mut enc, device);
        exec.execute_frame_with_gpu(graph, plan, frame_time(), &mut gpu);
    }
    enc.commit_and_wait_completed();

    let result = RenderTarget::new(device, w, h, FMT, "freeze-graph-result");
    let out_tex = exec
        .backend()
        .texture_2d(out_slot)
        .expect("graph output texture retained");
    {
        let mut e = device.create_encoder("freeze-graph-copy");
        e.copy_texture_to_texture(out_tex, &result.texture, w, h, 1);
        e.commit_and_wait_completed();
    }
    result
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FusedGainU {
    product: f32,
    _pad: [f32; 3],
}

/// Render the hand-fused Gain kernel: `out.rgb = in.rgb * product`, alpha kept.
/// One read, one multiply, one write — the bandwidth collapse of an N-Gain
/// chain.
fn render_fused_gain(device: &GpuDevice, input: &GpuTexture, product: f32) -> RenderTarget {
    let (w, h) = (input.width, input.height);
    let pipeline = device.create_compute_pipeline(
        include_str!("shaders/gain_fused.wgsl"),
        "cs_main",
        "freeze.gain_fused",
    );
    let out_rt = RenderTarget::new(device, w, h, FMT, "freeze-fused-out");
    let u = FusedGainU {
        product,
        _pad: [0.0; 3],
    };
    let mut enc = device.create_encoder("freeze-fused-exec");
    enc.dispatch_compute(
        &pipeline,
        &[
            GpuBinding::Bytes {
                binding: 0,
                data: bytemuck::bytes_of(&u),
            },
            GpuBinding::Texture {
                binding: 1,
                texture: input,
            },
            GpuBinding::Texture {
                binding: 3,
                texture: &out_rt.texture,
            },
        ],
        [w.div_ceil(16), h.div_ceil(16), 1],
        "freeze.gain_fused",
    );
    enc.commit_and_wait_completed();
    out_rt
}

/// Build the unfused `Source -> Gain(g1) -> Gain(g2) -> FinalOutput` chain and
/// render it. Returns (rendered texture, the source ResourceId is internal).
fn render_unfused_two_gain(device: &GpuDevice, input: &GpuTexture, g1: f32, g2: f32) -> RenderTarget {
    let mut g = Graph::new();
    let src = g.add_node(Box::new(Source::new()));
    let a = g.add_node(Box::new(Gain::new()));
    let b = g.add_node(Box::new(Gain::new()));
    let fout = g.add_node(Box::new(FinalOutput::new()));
    g.set_param(a, "gain", ParamValue::Float(g1)).unwrap();
    g.set_param(b, "gain", ParamValue::Float(g2)).unwrap();
    g.connect((src, "out"), (a, "in")).unwrap();
    g.connect((a, "out"), (b, "in")).unwrap();
    g.connect((b, "out"), (fout, "in")).unwrap();

    let plan = compile(&g).unwrap();
    let source_res = resource_for_output(&plan, src, "out");
    let output_res = resource_for_output(&plan, b, "out");
    render_graph(device, &mut g, &plan, source_res, input, output_res)
}

#[test]
fn fused_gain_chain_matches_unfused_within_tolerance() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient_input(&device, w, h);
    let (g1, g2) = (0.75_f32, 1.2_f32);

    let unfused = render_unfused_two_gain(&device, &input, g1, g2);
    let fused = render_fused_gain(&device, &input, g1 * g2);

    let differ = TextureDiff::new(&device);
    // abs 4e-3 / rel 1e-2: comfortably above the ~1 f16-ULP intermediate-
    // rounding drift, far below any real fusion error.
    let r = differ.compare(&device, &unfused.texture, &fused.texture, 4e-3, 1e-2);

    assert_eq!(
        r.over_count, 0,
        "correct fusion must clear the oracle (max_abs={}, max_rel={}, over={}/{})",
        r.max_abs, r.max_rel, r.over_count, r.total
    );
    assert!(
        r.max_abs < 4e-3,
        "the only diff should be sub-tolerance f16 accumulation, got max_abs={}",
        r.max_abs
    );
    assert!(r.passes(0.0), "correct fusion must pass the verdict at zero fraction");
}

#[test]
fn oracle_catches_wrong_fusion() {
    let device = crate::test_device();
    let (w, h) = (128u32, 128u32);
    let input = gradient_input(&device, w, h);
    let (g1, g2) = (0.75_f32, 1.2_f32);

    let unfused = render_unfused_two_gain(&device, &input, g1, g2);
    // Mis-fuse: product off by 1.5× — a real fusion bug the oracle MUST catch.
    let wrong = render_fused_gain(&device, &input, g1 * g2 * 1.5);

    let differ = TextureDiff::new(&device);
    let r = differ.compare(&device, &unfused.texture, &wrong.texture, 4e-3, 1e-2);

    assert!(
        r.over_count > 0,
        "oracle must flag a wrong fusion (max_abs={}, over={}/{})",
        r.max_abs, r.over_count, r.total
    );
    assert!(
        !r.passes(0.01),
        "a 1.5×-off fusion must fail the verdict (over_fraction={})",
        r.over_fraction()
    );
}
